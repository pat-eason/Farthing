# Architecture

How the app captures Claude Code usage data, why there are two data sources, and how the pieces fit. Audience: contributors and anyone deciding whether to trust the app with their `~/.claude/settings.json`.

## The one-paragraph version

Claude Code can export a `claude_code.api_request` OTel log event for every API request it makes, carrying authoritative `cost_usd` and all four token counts. The app embeds a loopback-only OTLP receiver, self-installs the five env vars that point Claude Code's logs exporter at it, and stores one SQLite row per request. Because OTel events don't say *where* a session ran, a `SessionStart` hook POSTs each session's `cwd` to the app. And because the live pipeline only captures what happens while the app is running (and only for sessions started after setup), the session transcripts Claude Code already writes under `~/.claude/projects/` are parsed as a second source: full history on first run, incremental gap recovery on every start. The two sources are deduplicated by `request_id`, which both carry verbatim.

## System overview

```
 Claude Code session                         this app (single Tauri process)
 ┌──────────────────────────┐               ┌───────────────────────────────────────┐
 │  OTel logs exporter      │   OTLP        │  receiver.rs (axum)                   │
 │  (claude_code.api_request│──http/json──▶ │  127.0.0.1:43177                      │
 │   events, ~5s batches)   │  POST /v1/logs│   ├─ ingest.rs ──▶ requests rows      │
 │                          │               │   └─ session.rs ─▶ sessions rows      │
 │  SessionStart hook       │   POST        │                                       │
 │  (curl, fail-silent)     │──/session───▶ │  db.rs: SQLite (WAL)                  │
 └──────────────────────────┘               │  ~/Library/Application Support/<app>/ │
                                            │  usage.db                             │
 ~/.claude/projects/**/*.jsonl              │                                       │
 (session transcripts)      ──read-only──▶  │  transcript.rs + backfill.rs          │
                                            │  (startup + manual passes,            │
                                            │   pricing.rs for cost)                │
                                            │                                       │
                                            │  queries.rs / metrics.rs              │
                                            │   ▲ Tauri commands (IPC)              │
                                            │  SvelteKit SPA:                       │
                                            │   tray popover + desktop views        │
                                            └───────────────────────────────────────┘
```

Everything runs in one process: the axum receiver, the SQLite store, the backfill engine, and the webview UI are all owned by the Tauri shell (`src-tauri/src/lib.rs` wires them together at startup).

## Two sources, one table: OTel-primary, transcript-backfill

Neither source alone is sufficient:

| | OTel `api_request` events | Transcript JSONL |
|---|---|---|
| `cost_usd` | **Yes, authoritative** (matches the CLI's own session cost to the digit) | No; must be computed from a pricing table |
| Token counts | All four (input/output/cache read/cache creation) | Same, plus the **5m/1h cache-creation TTL split** OTel lacks |
| `cwd` (project) | **No** | Yes |
| Coverage | Only while the app is running, and only sessions started after setup | Everything, but bounded by transcript retention (~30 days default) |
| Latency | Live (~5s export batches) | On backfill passes |

So the design is **OTel-primary**: live events are the row-level source of truth, ingested by `ingest.rs` with version-tolerant parsing (unknown fields ignored, missing required fields counted and surfaced in the health view, since the event schema is undocumented and may drift between Claude Code releases).

**Transcripts are backfill**, serving three jobs:

1. **Day-one history**: the first pass parses every transcript, so a fresh install renders charts immediately instead of an empty database.
2. **Gap recovery**: a pass runs on every app start (plus a manual "Backfill now"). Anything exported while the app was down (or paused) is recovered. `ingest_state` stores a byte offset per transcript file, so passes are incremental and idempotent.
3. **Enrichment**: the 5m/1h cache-creation split exists only in transcripts; backfill fills those nullable columns even on rows that arrived live.

Backfilled rows get `source='backfill'` and a locally computed cost; live rows get `source='otel'` and Claude Code's own `cost_usd`. The tag is visible in the UI for debuggability, and a **diff report** (health view) compares the two sources over a window to measure capture completeness against the <1% gap target.

### Dedup: why the two sources never double-count

Both sources carry the same identity: the OTel event's `request_id` attribute equals the transcript's `requestId` (`req_...`), verified field-for-field across live captures and with zero cross-session collisions in a 26,815-requestId corpus scan (full evidence: [notes/dedup-key.md](notes/dedup-key.md)).

Enforcement is a partial unique index, `idx_requests_request_id ON requests (request_id) WHERE request_id IS NOT NULL`, with conflict rules:

- **OTel rows win and are never overwritten**: they carry the authoritative cost. When backfill hits an existing otel row, it may only fill the transcript-exclusive columns (the 5m/1h split, and a missing `query_source`).
- When a live event arrives for a row backfill already inserted, the otel row replaces the backfill row's values.
- Transcript streaming writes multiple lines per request; `transcript.rs` collapses them (last non-zero-usage line wins) before insert, and skips synthetic no-API-traffic lines.
- A fuzzy fallback key (`session_id`, model, ±2s window, token signature) is documented for rows missing the id, but in practice every real request carries it.

## The session → cwd join

OTel events carry `session.id` but no working directory, so project attribution needs a second channel. Three mechanisms cooperate, all writing to the `sessions` table (`session_id PRIMARY KEY, cwd, first_seen_ms, last_seen_ms, source`):

1. **SessionStart hook (live)**: the installed hook POSTs Claude Code's hook stdin JSON (`session_id`, `cwd`) to `POST /session`. `session.rs` upserts the mapping (`source='hook'`, `first_seen_ms` preserved on repeats). The handler has a 50ms write budget; under DB contention it responds `202` and completes the write in the background, so the hook's 2s curl timeout is never approached.
2. **Backfill self-heal**: transcripts carry both `sessionId` and `cwd`, so any session whose hook POST was missed (app down, hook removed) is repaired on the next backfill pass.
3. **Query-time join**: all metrics are SQL aggregations over `requests LEFT JOIN sessions`; rows whose session has no known `cwd` group under "(unknown project)" rather than disappearing.

The hook is deliberately the *only* thing hooks are used for: Claude Code hooks carry no token/cost data (verified during design), so the hook channel exists purely for the `cwd` mapping.

## Self-installing configuration (`settings_merge.rs`, `onboarding.rs`)

The highest-blast-radius code in the app is the bit that edits `~/.claude/settings.json`. The exact keys written are quoted verbatim in the [README](../README.md#exactly-what-it-changes-in-settingsjson); the engine's rules:

- **Pure and path-agnostic**: `settings_merge.rs` is path-in/path-out and fixture-tested against real-world settings shapes; `onboarding.rs` resolves the real paths and gates the write behind a user-confirmed line diff plus conflict detection.
- **Ownership**: the app owns exactly five `env` keys and any `SessionStart` hook command containing its `/session` endpoint. Unmerge removes an env key only if it still holds the exact app value; a user-edited value means the user took ownership.
- **Never lossy**: timestamped backup before every write, atomic temp-file+rename, hard abort (no write at all) on malformed JSON or unexpected structure.
- **Conflicts surfaced, never silently resolved**: pre-existing OTel env vars pointing elsewhere require an explicit user acknowledgment.

The receiver port (43177) is fixed and never auto-rebound, because the endpoint baked into `settings.json` is literal; a port conflict is surfaced in the health view instead.

## Storage (`db.rs`)

SQLite via rusqlite, WAL mode, `busy_timeout` 5s, at `~/Library/Application Support/<bundle id>/usage.db`. Migrations are embedded and versioned through `meta.schema_version` (currently v4). Tables:

- `requests` — one row per API request (or `api_error`): identity, timestamps, model, `query_source` (main vs subagent), cost, the four token counts, nullable 5m/1h cache split, `source` tag.
- `sessions` — the session→cwd mapping described above.
- `ingest_state` — per-transcript-file byte offsets for incremental backfill.
- `meta` — schema version, persisted app state (e.g. capture-pause flag).

Two wide covering indexes (time-leading and session-leading) keep every UI aggregation an index-only scan; the seeded 1M-row gate holds the worst warm query under 500ms. Day boundaries are computed at local midnight (DST-correct) in `metrics.rs`.

## Pricing (`pricing.rs`)

Backfilled rows need a cost that transcripts don't store. `PricingTable` layers three sources: a bundled snapshot of LiteLLM's model-pricing JSON (so the app works offline forever), a locally cached copy, and a fail-silent refresh on startup from the pinned LiteLLM URL (validated before it can replace the cache). Unknown models produce `cost_usd = NULL`, rendered as tokens-only with an "unpriced" marker rather than a fake $0. Live otel rows never touch the pricing table.

## UI layer

SvelteKit (static adapter, SPA) in the Tauri webview; all data access goes through Tauri commands (registered in `lib.rs`, thin TS wrappers in `src/lib/*.ts`).

- **Tray** (`tray.rs`): menu bar icon owning a popover window (positioned under the icon) and a hidden main window. Activation policy flips Accessory↔Regular as the desktop window opens/closes, so there's no Dock icon in menu-bar-only mode.
- **Popover** (`/popover`): today's metrics + sparkline. Updates live: `ingest.rs` emits an `ingest:stored` Tauri event after each export batch that stores rows, and the popover debounce-refetches instead of polling.
- **Desktop** (`/cost`, `/sessions`, `/tokens`, `/projects`, `/health`, `/settings`): faceted analysis views sharing one facet state (range/project/model/query-source) pushed down into SQL by `queries.rs`.
- **Pause/resume** (`capture.rs`): the receiver keeps returning 200 but discards events; the pause is persisted in `meta` and the paused window is recoverable later via backfill.

## Reliability posture

- The app must never slow down or break Claude Code: the hook is fail-silent (`|| true`, 2s timeout), OTLP export failures are absorbed by Claude Code's exporter, and every gap is recoverable from transcripts.
- The receiver returns 400 only on malformed bodies; unknown event shapes are accepted and counted as ingest failures for the health view, never bounced.
- Health view (`health.rs`) diagnoses the "configured but no events" case (e.g. sessions predating setup, port stolen) with cause attribution.

## Module map

| Module (`src-tauri/src/`) | Responsibility |
|---|---|
| `lib.rs` | Startup wiring: state, receiver spawn, backfill pass, tray |
| `receiver.rs` | axum server, `/v1/logs`, `/v1/metrics` (accept/discard), `/session`, port-conflict status |
| `ingest.rs` | OTLP JSON → `requests` rows, version-tolerant, failure counters, live-update event |
| `session.rs` | SessionStart hook payload → `sessions` upsert |
| `settings_merge.rs` | Pure settings.json merge/unmerge engine |
| `onboarding.rs` / `uninstall.rs` | User-confirmed apply/reverse flows around the engine |
| `transcript.rs` | JSONL parsing + per-request line collapse |
| `backfill.rs` | Pass orchestration, offsets, dedup-aware inserts, diff report |
| `pricing.rs` | Bundled/cached/refreshed pricing, cost computation |
| `db.rs` | Connection, migrations, schema |
| `metrics.rs` / `queries.rs` | Popover rollups / faceted desktop queries |
| `tray.rs` / `capture.rs` / `autostart.rs` / `health.rs` | Menu bar shell, pause, LaunchAgent, diagnostics |

Decision records with the underlying evidence live in [docs/notes/](notes/): the [OTel schema findings](notes/otel-schema.md) (env-var matrix, event shape), the [dedup key spike](notes/dedup-key.md), [pricing](notes/pricing.md), and [naming](notes/naming.md).
