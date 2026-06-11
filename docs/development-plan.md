# Development Plan: Claude Usage Tracker (working name)

> **Generated from:** docs/prd.md
> **Created:** 2026-06-11
> **Last synced:** 2026-06-11
> **Status:** Active Planning Document
> **VibeKanban Project ID:** [To be assigned]

## Overview

A single open-source Tauri macOS app that makes Claude Code token/cost usage visible with zero manual configuration. It embeds a localhost OTLP receiver ingesting Claude Code's `claude_code.api_request` telemetry events into SQLite, self-installs the required `~/.claude/settings.json` configuration (env block + SessionStart hook for session→cwd mapping), backfills history from transcript JSONL files, and surfaces metrics via a menu bar popover and a full desktop UI — facetable by project, model, session, and cache type.

## Tech Stack

- **Backend:** Rust (Tauri v2) — axum OTLP receiver, rusqlite
- **Frontend:** TypeScript + Svelte; uPlot or LayerCake for charts
- **Database:** SQLite (WAL mode) at `~/Library/Application Support/<app>/usage.db`
- **Infrastructure:** GitHub Actions (build, sign, notarize, release); `tauri-plugin-autostart` (LaunchAgent)

---

## Completion Status Summary

| Epic | Status | Progress |
|------|--------|----------|
| 1. Capture Pipeline | Not Started | 0% |
| 2. Self-Install & Lifecycle | Not Started | 0% |
| 3. Transcript Backfill | Not Started | 0% |
| 4. Menu Bar UI | Not Started | 0% |
| 5. Desktop UI | Not Started | 0% |
| 6. OSS Release | Not Started | 0% |

---

## Epic 1: Capture Pipeline (NOT STARTED)

Foundation: Tauri scaffold, the embedded OTLP receiver, event parsing, the SQLite schema, and the session→cwd mapping endpoint. Exit state: with manually-configured env vars, a real Claude Code session produces queryable rows in SQLite. No UI beyond a stub window.

### Acceptance Criteria

- [ ] A real Claude Code session (manually configured to export OTLP) results in correct per-request rows in `usage.db`
- [ ] Receiver binds `127.0.0.1:43177` only; non-loopback connections are rejected
- [ ] `session_id → cwd` mappings are captured via `POST /session`
- [ ] Unknown/extra fields in OTel payloads never crash ingestion (version-tolerant parsing)

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 1.1 | Scaffold Tauri v2 app | Tauri v2 + Svelte/TS template, repo init, basic window, dev/build scripts, CI build check | High | M | — | done <!-- vk: --> |
| 1.2 | SQLite layer & schema | rusqlite + WAL, App Support path resolution, embedded migrations, tables: `requests`, `sessions`, `ingest_state`, `meta` | High | M | 1.1 | done <!-- vk: --> |
| 1.3 | axum OTLP receiver | Localhost-only server on fixed port 43177, `POST /v1/logs`, `POST /v1/metrics` (accept + discard), port-in-use detection (no auto-rebind) | High | M | 1.1 | done <!-- vk: --> |
| 1.4 | OTel event ingestion | Parse `claude_code.api_request` / `api_error` from OTLP `http/json` into `requests` rows (cost_usd, 4 token counts, model, query_source, session.id, ts); version-tolerant | High | M | 1.2, 1.3 | done <!-- vk: --> |
| 1.5 | Session mapping endpoint | `POST /session` accepting SessionStart hook stdin JSON; upsert `session_id → cwd` into `sessions` | High | S | 1.2, 1.3 | done <!-- vk: --> |
| 1.6 | End-to-end pipeline verification | Manually configure a Claude Code session against the receiver; assert row counts/values vs the session transcript; document findings | High | M | 1.4, 1.5 | done <!-- vk: --> |

### Task Details

**1.1 - Scaffold Tauri v2 app**
- [x] `pnpm tauri dev` opens a window; `pnpm tauri build` produces a .app with no errors
- [x] Repo layout: `src/` (Svelte), `src-tauri/` (Rust), `docs/`, lint + format configs for both languages
- [x] GitHub Actions workflow runs check/clippy/build on push

**1.2 - SQLite layer & schema**
- [x] DB created at `~/Library/Application Support/<app>/usage.db` on first boot; WAL mode confirmed via pragma
- [x] Migrations are embedded, versioned in `meta`, and idempotent across restarts
- [x] `requests` has indexes covering (timestamp), (session_id), (model); `sessions` keyed on session_id
- [x] Unit tests cover fresh-create, re-open, and migration-upgrade paths

**1.3 - axum OTLP receiver**
- [x] Server binds `127.0.0.1:43177`; connection attempts from a non-loopback address fail
- [x] `POST /v1/logs` and `POST /v1/metrics` return 200 for well-formed OTLP `http/json`; metrics payloads are discarded
- [x] Port-in-use at startup is detected and exposed as queryable app state (no auto-rebind)
- [x] Malformed JSON returns 400 without panicking

**1.4 - OTel event ingestion**
- [x] `claude_code.api_request` events insert rows with cost_usd, input/output/cache_read/cache_creation tokens, model, query_source, session.id, timestamp
- [x] `claude_code.api_error` events are stored with error metadata
- [x] Unknown event names and unknown attributes are ignored without error; missing required fields increment a visible ingest-failure counter (`ingest_stats` command)
- [x] Fixture tests use captured real OTLP payloads (record one during development) — real `api_request` batch captured from Claude Code v2.1.173, sanitized in `src-tauri/tests/fixtures/` (api_error fixture is reconstructed; see fixtures README)

**1.5 - Session mapping endpoint**
- [x] `POST /session` with SessionStart hook JSON upserts (session_id, cwd, first_seen, source='hook')
- [x] Repeat POSTs for the same session_id are idempotent (first_seen preserved, single row; missing `cwd` never clobbers a stored one)
- [x] Responds within 100ms and never blocks on DB contention (hook curl has a 2s timeout) — handler waits ≤50ms for the write, then responds 202 and lets the write land in the background; covered by a contention test

**1.6 - End-to-end pipeline verification**
- [x] With env vars set manually in a shell, a Claude Code session produces ≥1 row per API request in `requests` — verified via `cargo run --example e2e_receiver` + real `claude -p` runs (1-request turn → 1 row; 2-request tool-use turn → 2 rows). Signal-specific `OTEL_EXPORTER_OTLP_LOGS_{PROTOCOL,ENDPOINT}` required; generic vars alone export nothing
- [x] Row token counts reconcile with the session's transcript `message.usage` values — all 4 token counts exact on all 3 requests; `SUM(cost_usd)` matches CLI `total_cost_usd` to the digit
- [x] `sessions` contains the session's cwd mapping via the manually-configured hook (`--settings` file; all 5 e2e sessions mapped, source='hook')
- [x] Findings (event schema observations, request-identity fields seen) written to `docs/notes/otel-schema.md` — feeds spike 3.1 (`request_id` == transcript `requestId` exactly; transcript lines not 1:1 with requests)

---

## Epic 2: Self-Install & Lifecycle (NOT STARTED)

The zero-config promise: safe settings.json merge/unmerge, onboarding with diff preview and conflict detection, LaunchAgent autostart, uninstall, and the health view. Highest-blast-radius epic — fixture tests are non-negotiable.

### Acceptance Criteria

- [ ] Fresh machine: install app → click through onboarding → restart Claude Code session → data flows, with zero manual file edits
- [ ] settings.json modifications are previewed, backed up, strictly additive to app-owned keys, and fully reversible
- [ ] Pre-existing OTel/telemetry config is detected and never silently overwritten
- [ ] Health view accurately reports receiver, config, and data-flow state

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 2.1 | settings.json merge engine | Read/parse, deep-merge of env block + SessionStart hook entry, timestamped backup, strict unmerge of app-owned keys only; fixture test suite | High | L | 1.1 | done <!-- vk: --> |
| 2.2 | Onboarding flow | First-run UI: detect config state, render diff preview, conflict detection (existing OTel vars), apply merge, "restart your sessions" notice | High | M | 2.1 | done <!-- vk: --> |
| 2.3 | Autostart (LaunchAgent) | `tauri-plugin-autostart` integration; enabled during onboarding, toggleable in settings | High | S | 1.1 | done <!-- vk: --> |
| 2.4 | Uninstall flow | Reverse merge, remove LaunchAgent, optional DB deletion, confirmation UX | Medium | M | 2.1, 2.3 | done <!-- vk: --> |
| 2.5 | Health & diagnostics view | Receiver status, last-event-at, config installed/missing/conflicting, port conflict surfacing, "configured but no events" detector with causes | Medium | M | 1.4, 2.2 | done <!-- vk: --> |

### Task Details

**2.1 - settings.json merge engine**
- [x] Merge adds exactly: 5 `env` keys + 1 `SessionStart` hook entry; all other content byte-preserved (key order/formatting changes acceptable, data loss is not) — `src-tauri/src/settings_merge.rs` `APP_ENV` (1.6-verified 4-key block + generic `OTEL_EXPORTER_OTLP_PROTOCOL`, see `docs/notes/otel-schema.md`); key order kept via serde_json `preserve_order`
- [x] Timestamped backup written to a backups dir before any write; restore-from-backup function works — `settings-<UTC ts>.json`, byte-exact copy, collision-safe; no-op merges take no backup
- [x] Unmerge removes only app-owned keys/hook entries and leaves user content intact, including when the user edited adjacent config after install — env keys removed only at the exact app value (user-edited value = user ownership); hooks matched on the `/session` endpoint marker
- [x] Fixture suite covers: missing file, empty file, real-world large settings.json, pre-existing env block, pre-existing SessionStart hooks, malformed JSON (abort, never write) — 20 tests, fixtures in `src-tauri/tests/fixtures/settings/` (realworld.json is an anonymized real settings.json)

**2.2 - Onboarding flow**
- [x] First launch shows a literal before/after diff of settings.json changes; nothing is written until the user confirms — `src-tauri/src/onboarding.rs` (`onboarding_status`: read-only line diff via `similar`) + preview screen in `src/routes/+page.svelte`; verified live against a scratch file (`CLAUDE_USAGE_TRACKER_SETTINGS_PATH` dev override)
- [x] Existing `OTEL_*` / `CLAUDE_CODE_ENABLE_TELEMETRY` values trigger a conflict screen requiring an explicit choice — conflict table with "Overwrite and continue" / "Cancel, change nothing"; backend `onboarding_apply` refuses unless `acknowledge_conflicts=true`
- [x] Post-apply screen instructs restarting running Claude Code sessions and links to the health view — done screen shows backup path + restart notice + link to `/health` (stub view with receiver status; full diagnostics in 2.5)
- [x] Re-running onboarding on an already-configured machine is a no-op with a "already configured" state — `changed=false` routes to the configured screen; re-apply writes nothing and takes no backup (tested + verified live across an app relaunch)

**2.3 - Autostart (LaunchAgent)**
- [x] App relaunches on login after enabling; LaunchAgent plist present — `tauri-plugin-autostart` in `MacosLauncher::LaunchAgent` mode: `enable()` writes `~/Library/LaunchAgents/<app>.plist`; auto-enabled after onboarding apply (`onboarding_apply` → `autostart::enable_after_onboarding`, best-effort, never fails the merge). Per task constraint, verified at the plugin-API level (MockRuntime tests in `src-tauri/src/autostart.rs` read real plugin state), not via a live login on this machine: dev builds deliberately refuse `enable()` so a LaunchAgent never points at the dev binary; live relaunch check belongs to release-build validation
- [x] Toggle off removes the LaunchAgent; state reflected accurately in settings UI — `/settings` view with start-at-login toggle; `autostart_set(false)` removes the plist only when registered (idempotent), and the UI always re-reads `autostart_status` (live `is_enabled()`, never cached) after every action, including refusals/errors

**2.4 - Uninstall flow**
- [x] Uninstall removes app-owned settings.json entries, the LaunchAgent, and (only if opted-in) the database — `src-tauri/src/uninstall.rs` `uninstall_apply(delete_database)`: strict unmerge gates everything (a settings error aborts with nothing touched), then best-effort LaunchAgent removal (re-reads real state on failure) and opt-in deletion of `usage.db` + `-wal`/`-shm` sidecars under the DB mutex (safe with the connection open; data vanishes when the app exits). settings.json backups are deliberately kept
- [x] A Claude Code session started post-uninstall exports nothing and logs no hook errors — verified live against a scratch settings file: merge → unmerge restored the exact user content, then `claude -p --settings <unmerged>` with the production receiver on 43177 yielded 0 requests/0 sessions rows and a completely empty stderr; control run with the merged file on the same harness produced 1 request + 1 session row
- [x] Confirmation dialog states exactly what will and won't be removed — `uninstall_status` drives the dialog in `src/routes/settings/+page.svelte`: will-remove list (settings entries with the literal line diff, LaunchAgent live state, opt-in DB checkbox with size/path) and won't-remove list (rest of settings.json, backups dir, the app bundle itself with quit-and-Trash instructions)

**2.5 - Health & diagnostics view**
- [x] Shows: receiver listening (or port-conflict error), last event received timestamp, settings.json state (installed/missing/conflicting), backfill progress — `src-tauri/src/health.rs` `health_status` command aggregates everything in one read-only query, rendered by the full `/health` view (replaces the 2.2 stub, auto-refreshes every 5s). Last-event-at is the freshest of the in-memory ingest clock and the newest stored `requests` row, so it survives app restarts; settings.json state is derived live (installed / missing / conflicting with the conflict list / unreadable). Backfill progress renders the `not_available` placeholder until the Epic 3 engine ships (the `BackfillStatus` enum is the extension point). Every state verified rendered in-browser against the serialization contract locked by `health_serializes_for_frontend`
- [x] "Configured but no events in N minutes" state lists likely causes (sessions predate config, port conflict, paused) — `diagnose_no_events`: fires only when config is installed and nothing arrived in 10 minutes (or ever); a broken receiver (port conflict / failed) is reported as the definitive cause, otherwise the ambiguous pair (restart pre-config sessions, or Claude Code simply not in use) with remediation text
- [x] Ingest-failure counter from 1.4 surfaced here — `IngestStatsSnapshot` embedded in the health payload; failures render highlighted with a "please report this" note, plus events-stored/ingested/skipped context counters

---

## Epic 3: Transcript Backfill (NOT STARTED)

Day-one history and gap recovery: JSONL parsing, pricing, dedup against live rows, and incremental offset-based recovery. Starts with the dedup-identity spike flagged in the PRD review.

### Acceptance Criteria

- [ ] First launch populates all available transcript history (up to ~30 days) with computed costs
- [ ] Live OTel rows and backfilled rows never double-count the same activity
- [ ] Sessions missed while the app was down (including their cwd mappings) are recovered on next start
- [ ] Backfill diff report quantifies capture completeness (<1% missing target)

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 3.1 | Spike: dedup identity | Verify whether `api_request` OTel events carry an identifier matching transcript `requestId`; decide exact dedup key (exact-id vs session_id+ts-window+token-signature); document | High | S | 1.6 | done <!-- vk: --> |
| 3.2 | Transcript JSONL parser | Parse `~/.claude/projects/**/*.jsonl`: per-assistant-message usage (incl. 5m/1h cache split), model, sessionId, cwd, timestamp, isSidechain; tolerant of unknown line types | High | M | — | done <!-- vk: --> |
| 3.3 | Pricing table | Bundled versioned per-model pricing data + fail-silent remote refresh (pinned LiteLLM URL, local cache); cost computation incl. cache read/write multipliers; unknown-model → tokens-only flag | High | M | — | done <!-- vk: --> |
| 3.4 | Backfill engine | First-run full pass + incremental passes via stored byte offsets; dedup per 3.1; source tagging (otel/backfill); session→cwd self-heal from transcripts | High | L | 1.2, 3.1, 3.2, 3.3 | done <!-- vk: --> |
| 3.5 | Backfill diff report & manual trigger | "Backfill now" action; report comparing OTel rows vs transcript ground truth over a window (capture-completeness metric) | Medium | S | 3.4 | done <!-- vk: --> |

### Task Details

**3.1 - Spike: dedup identity**
- [x] Side-by-side comparison of ≥3 real sessions' OTel event payloads vs transcript entries, focused on request-identity fields — 4 sessions / 6 requests total: 2 sessions from the 1.6 e2e runs plus 2 fresh controlled headless runs whose raw OTLP bodies were captured via a tee proxy in front of the production receiver; `request_id` == transcript `requestId` exactly in every case, token fields identical. Backed by a corpus scan of all 481 local transcript files (26,815 distinct requestIds, zero cross-file collisions)
- [x] Decision written to `docs/notes/dedup-key.md`: exact key, collision behavior, and fallback strategy — exact `request_id` with partial unique index, otel-wins-on-conflict (authoritative `cost_usd`; backfill may fill only the 5m/1h cache split), fuzzy (session_id, model, ±2s window, token signature) demoted to fallback for id-less rows; plus transcript-collapse rules for 3.2/3.4 (last non-zero line per requestId, skip `<synthetic>`)
- [x] PRD FR-4 dedup bullet updated with the verified answer — FR-4 bullet and the double-counting risk row now state the verified `request_id` key

**3.2 - Transcript JSONL parser**
- [x] Extracts input/output/cache_read/cache_creation (with ephemeral_5m/1h breakdown), model, sessionId, cwd, timestamp, isSidechain from assistant messages — `src-tauri/src/transcript.rs`: `parse_file`/`parse_file_from(path, offset)`/`parse_reader` → `AssistantUsage` per line (RFC 3339 → unix ms, numeric-string-tolerant counts, 5m/1h split `None` when absent), plus `collapse_requests` applying the 3.1 rules (last non-zero-usage line per `requestId`, `<synthetic>`/id-less lines dropped — including the corpus case where a synthetic all-zero line *carries* the requestId). `bytes_consumed` excludes a trailing unterminated line: the byte-offset contract for 3.4 incremental passes
- [x] Skips non-assistant lines and unknown line types without error; malformed lines counted, not fatal — any non-`assistant` `type` (known or future) → `skipped_lines`; invalid JSON / non-objects → `malformed_lines`; assistant lines missing sessionId/timestamp/usage → `invalid_assistant_lines`. Verified beyond fixtures with a full read-only corpus run (`cargo run --example parse_transcripts -- ~/.claude/projects`): 1,314 files / 186,857 lines / 84,963 assistant lines / 43,087 collapsed requests == distinct requestIds, 0 malformed, 0 invalid — every count exactly matching an independent Python scan of a frozen snapshot
- [x] Fixture tests against real transcript files (sanitized) covering main + sidechain messages — `tests/fixtures/transcripts/{main-session,sidechain,edge-cases}.jsonl`, all derived line-for-line from real transcripts (usage/ids/timestamps verbatim, content redacted; provenance in the fixtures README): main session with 2 streaming groups, subagent sidechain lines (`isSidechain: true`, 5m-split), and the 3.1 corpus oddities (cumulative growth 5→1004, trailing synthetic-with-requestId, requestId-less synthetic, unknown future type, truncated line). 11 new tests; 99 total green

**3.3 - Pricing table**
- [x] Bundled data file covers current model families; computation applies cache-read (~0.1×) and cache-write (1.25×/2× by TTL) multipliers — `src-tauri/data/pricing-bundled.json` (`include_str!` into `src-tauri/src/pricing.rs`): 22 Anthropic models filtered from a pinned LiteLLM snapshot (commit + date in the file's `_claude_usage_tracker` version entry; provenance/regeneration in `docs/notes/pricing.md`), covering every model in the local transcript corpus plus claude-3/4/4.5 generations. Rates use the entry's explicit cache cost fields (asserted to sit exactly on 0.1×/1.25×/2×); missing fields fall back to the multipliers; unsplit cache-creation tokens price at the 5m rate (Claude Code's default TTL), split tokens at their per-TTL rates
- [x] Remote refresh is fail-silent with timeout, validates schema before replacing cache, never blocks app start — `pricing::refresh` is spawned (never awaited) in `lib.rs` setup after the synchronous, network-free `PricingTable::load` (bundled + cache overlay); 10s whole-request timeout; payload must be a JSON object with ≥1 well-formed anthropic `claude-*` entry before the cache file (atomic write-then-rename) or in-memory table is touched — bad JSON/shape/empty payloads and unreachable hosts leave both bit-identical (tested against a local axum server), and a corrupt cache file on disk is ignored at load. Live-verified against the real pinned URL over TLS: `cargo run --example refresh_pricing` (22 models fetched → cache written → reloaded)
- [x] Unknown model → row stored with null cost + flagged; surfaced as tokens-only in UI — `cost_for` returns `CostOutcome::UnknownModel` (`usd() == None`, shaped for the nullable `requests.cost_usd` column) for unknown/`<synthetic>`/missing models; this is the 3.4 storage contract (backfill row with `NULL` cost *is* the tokens-only flag — OTel rows always carry exporter-computed cost) which the Epic 4/5 cost views consume; lookup normalizes provider prefixes and date-suffix variants first so only genuinely unknown models go unpriced
- [x] Computed costs for a known fixture session match hand-calculated values — `fixture_session_costs_match_hand_calculated_values`: both collapsed requests of `tests/fixtures/transcripts/main-session.jsonl` (claude-fable-5, 1h-TTL cache writes) priced via `collapse_requests` → `cost_for`, asserted to <1e-12 against literal hand arithmetic ($0.825931 and $0.567567); 14 new tests, 113 total green

**3.4 - Backfill engine**
- [x] Fresh install: full pass ingests all transcripts; re-running immediately ingests zero new rows (offset idempotency) — `src-tauri/src/backfill.rs`: `run_pass` walks `~/.claude/projects/**/*.jsonl` (env-overridable via `CLAUDE_USAGE_TRACKER_PROJECTS_DIR`), parses each file from its stored `ingest_state.byte_offset` (rows + offset advance in one per-file transaction), and is spawned on a blocking thread at every app start in `lib.rs`. Live full-corpus run (`cargo run --example backfill_pass -- ~/.claude/projects <tmp-db>`): 1,323 files → 43,403 requests inserted, **all 43,403 priced** ($6,212.73 total, 0 unknown-model), 432 sessions with cwd, 12.0s; immediate second pass inserted 0 rows in 87ms (one `stat` per unchanged file, no opens). A file shorter than its offset (rotation/truncation) resets to 0 and re-reads idempotently
- [x] Simulated gap (app down during a live session) is fully recovered on next start with no double-counting against earlier live rows — dedup on exact `request_id` enforced by schema-v2 partial unique index (`idx_requests_request_id`, with a v1-duplicate collapse in the migration); backfill defers to any existing row (otel wins: cost untouched, only a NULL 5m/1h cache split is filled in) and the mirror-image race (backfill beats a still-in-flight request's export) is handled in `ingest.rs` via `ON CONFLICT … DO UPDATE … WHERE source = 'backfill'` takeover that preserves the transcript-exclusive split. Covered by `simulated_gap_recovers_missed_requests_without_double_counting`, `incremental_pass_picks_up_lines_appended_after_the_offset`, `otel_takes_over_an_existing_backfill_row_keeping_the_cache_split`, and `redelivered_batch_does_not_duplicate_the_request_row`
- [x] `sessions` rows missing cwd (hook POST missed) are healed from transcript data — per-file session aggregation (first non-NULL `cwd`, min/max line timestamps) upserts `sessions`: missing sessions created with `source='backfill'`, an existing row's NULL `cwd` filled via `COALESCE(sessions.cwd, excluded.cwd)`, first/last-seen widened; hook data is never overwritten and `source='hook'` never downgrades. Live run healed all 432 corpus sessions' cwds from scratch; `sessions_missing_cwd_are_healed_and_hook_data_is_preserved` covers both heal and don't-clobber
- [x] Rows carry source tag (`otel` | `backfill`) — backfill inserts `source='backfill'` (live ingest already wrote `'otel'`; the v1 CHECK constraint admits only these two); per-source counts asserted in tests and reported by the example harness. `BackfillState`/`backfill_status` command expose the last `BackfillSummary` (files/requests/sessions/parse counters) for 3.5's manual trigger and report; 125 tests green, clippy/fmt/frontend checks clean

**3.5 - Backfill diff report & manual trigger**
- [x] "Backfill now" runs an incremental pass on demand with progress feedback — `backfill_run` command runs the same incremental `run_pass` on a blocking thread via `backfill::run_manual`, which atomically claims the `running` flag and refuses while another pass (startup or manual) is mid-flight. Progress feedback: `health_status` now carries the live `BackfillInfo` (running flag + last pass summary; the 2.5 `not_available` placeholder is gone) and the health view's Backfill card shows the running state, disables the button mid-pass, reports the recovered-request count on completion, and renders the last-pass summary line (files read, requests recovered/already-captured, sessions healed)
- [x] Report outputs: rows in OTel-only, backfill-only, matched; percentage missing vs transcript ground truth — `backfill_diff_report(window_hours)` (24h/7d/30d selector in the UI) re-parses transcripts from byte 0 (ground truth must not depend on stored ingest offsets; mtime-pruned to the window) and compares collapsed requestIds against stored `source='otel'` api_request rows: matched / backfill-only (missed live, recovered) / otel-only (transcript since cleaned up), with `missing_pct = backfill_only / transcript_requests` rendered against the <1% PRD target; a ±10-min edge band absorbs OTel-vs-transcript timestamp skew at the window boundary. Live-verified read-only against the real corpus (`cargo run --example diff_report`): 1,333 files, 14,274 ground-truth requests in the trailing 7 days; all-backfill DB → 100% missing / 0 matched, 200 rows flipped to otel → exactly 200 matched / 0 otel-only, one orphan otel row → otel-only = 1. 130 tests green, clippy/fmt/frontend checks clean

---

## Epic 4: Menu Bar UI (NOT STARTED)

The daily-driver surface: tray icon, popover with today's metrics, sparkline, live updates, pause/resume.

### Acceptance Criteria

- [ ] Popover shows today's cost, token split, session count, 7/30-day sparkline, top 3 projects — rendering in <100ms
- [ ] Values update near-real-time (~5s) as events arrive
- [ ] No Dock icon in menu-bar-only mode
- [ ] Pause discards incoming events (200 + drop) with a visible paused badge

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 4.1 | Tray icon & popover shell | Tauri v2 TrayIcon, popover window anchored to tray, `ActivationPolicy::Accessory`, menu (open app / pause / quit) | High | M | 1.1 | done <!-- vk: --> |
| 4.2 | Today-metrics queries & popover content | SQL aggregations (local-midnight day boundary, distinct session_ids), popover layout: cost, in/out/cache tokens, sessions, top 3 projects | High | M | 1.4, 4.1 | <!-- vk: --> |
| 4.3 | Sparkline | 7/30-day cost sparkline in popover (uPlot/LayerCake) | Medium | S | 4.2 | <!-- vk: --> |
| 4.4 | Live updates & pause/resume | Rust→frontend event push on ingest; pause state (receiver 200+discard), paused badge, resume | Medium | M | 4.2 | <!-- vk: --> |

### Task Details

**4.1 - Tray icon & popover shell**
- [x] Tray icon visible; click opens popover positioned at the tray; click-away dismisses — live-verified with synthetic CGEvent clicks on the status item: popover shows anchored under the icon (screenshot-confirmed), a click elsewhere hides it, and a second tray click closes instead of reopening (300ms auto-hide suppression window)
- [x] No Dock icon while only the popover exists; Dock icon appears when the desktop window opens and disappears when it closes — `lsappinfo` ApplicationType observed as `UIElement` at startup and with the popover open, `Foreground` after the open-app menu item, back to `UIElement` after closing the window (close is intercepted: hide + policy flip, app stays resident)
- [x] Menu actions wired: open desktop app, pause/resume (stub ok until 4.4), quit — all three exercised via the real tray menu: open shows + focuses `main`, "Pause capture" check state toggles and mirrors into `TrayState::paused` (receiver hookup is 4.4), quit exits the process

**4.2 - Today-metrics queries & popover content**
- [ ] Day = local midnight boundary; sessions = distinct session_ids active today (resume doesn't double-count)
- [ ] Popover shows cost, input/output/cache-read/cache-creation tokens, session count, top 3 projects by cost
- [ ] Renders in <100ms against a DB seeded with 100k+ rows
- [ ] Cost labeled "API-equivalent"

**4.3 - Sparkline**
- [ ] 7-day and 30-day toggle; bars/line match SQL aggregation values exactly
- [ ] Renders correctly with sparse data (gaps, single day, empty)

**4.4 - Live updates & pause/resume**
- [ ] New ingested event updates popover values within one export interval (~5s) without reopening
- [ ] Pause: receiver returns 200 and discards; tray shows paused badge; resume restores ingestion
- [ ] Paused state persists across app restart

---

## Epic 5: Desktop UI (NOT STARTED)

The analysis surface: faceted query layer and the four main views. Can develop against seeded fixture data in parallel with Epics 2–3.

### Acceptance Criteria

- [ ] All four views (cost over time, sessions, tokens/cache, projects) functional with global facets: project, model, date range, query_source
- [ ] Queries over ~1M rows return in <500ms
- [ ] Cost consistently labeled API-equivalent; tokens-only rows (unknown pricing) handled visibly

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 5.1 | Desktop window shell & navigation | Full window from tray, view navigation, activation policy flip, dark/light | High | M | 4.1 | <!-- vk: --> |
| 5.2 | Faceted query layer | Tauri commands wrapping SQL aggregations with shared facet params (project, model, date range, query_source); seed-data script for dev | High | M | 1.2 | <!-- vk: --> |
| 5.3 | Cost-over-time view | Line/bar chart, day/week/month/all ranges, stack by model or project | High | M | 5.1, 5.2 | <!-- vk: --> |
| 5.4 | Sessions view | Sortable table (cost, tokens, duration, project, models) + per-session detail drill-in | Medium | M | 5.1, 5.2 | <!-- vk: --> |
| 5.5 | Tokens & cache view | In/out/cache-read/cache-creation over time; cache hit-rate trend | Medium | M | 5.1, 5.2 | <!-- vk: --> |
| 5.6 | Projects view | Per-directory rollups (cost, tokens, sessions), cleaned path display | Medium | S | 5.1, 5.2 | <!-- vk: --> |

### Task Details

**5.1 - Desktop window shell & navigation**
- [ ] Window opens from tray menu and popover; closing returns to Accessory mode
- [ ] Navigation between the four views preserves active facet selections
- [ ] Respects system dark/light appearance

**5.2 - Faceted query layer**
- [ ] One shared facet struct (project, model, date range, query_source) applied across all aggregation commands
- [ ] Queries on a 1M-row seeded DB return <500ms (indexes verified with `EXPLAIN QUERY PLAN`)
- [ ] Seed script generates realistic multi-project/multi-model/multi-week fixture data
- [ ] Unit tests assert aggregation correctness against hand-computed fixtures

**5.3 - Cost-over-time view**
- [ ] Range presets (day/week/month/all) and custom range work; stacking toggles between model and project
- [ ] Chart totals reconcile exactly with the sessions/projects views for the same facets
- [ ] Empty-state and sparse-data render sensibly

**5.4 - Sessions view**
- [ ] Table sortable by cost, tokens, duration, start time; facets apply
- [ ] Session detail shows per-request timeline, model mix, cache behavior, source tags
- [ ] Sessions with no cwd mapping display as "unknown project," not errors

**5.5 - Tokens & cache view**
- [ ] Four token series charted over time with same range controls as 5.3
- [ ] Cache hit-rate trend (cache_read / (cache_read + input)) defined in UI copy and matches SQL
- [ ] 5m vs 1h cache-creation split visible where backfill data provides it

**5.6 - Projects view**
- [ ] Directories rolled up with cost, tokens, session counts; sorted by cost
- [ ] Paths displayed cleaned (`~/...`); click-through applies that project as a global facet

---

## Epic 6: OSS Release (NOT STARTED)

Public-readiness: naming, signing/notarization, CI releases, docs, final hardening, and the v1.0 cut.

### Acceptance Criteria

- [ ] Notarized .dmg installs and passes Gatekeeper on a clean machine
- [ ] Tagged release builds, signs, notarizes, and publishes via GitHub Actions
- [ ] README/docs sufficient for a stranger to install, trust the settings.json behavior, and contribute

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 6.1 | Name & branding | Final name (avoid ccusage/CCSeva collision), icon set, bundle identifier | Medium | S | — | <!-- vk: --> |
| 6.2 | Signing, notarization & release CI | Developer ID signing, notarization, GitHub Actions tag→.dmg release pipeline | High | L | 1.1 | <!-- vk: --> |
| 6.3 | Docs & license | README (incl. exact settings.json changes made), architecture doc, contribution guide, LICENSE | Medium | M | 6.1 | <!-- vk: --> |
| 6.4 | Onboarding polish & hardening | First-run UX pass, error/edge-case sweep (locked files, permission denials, odd configs), copy review | Medium | M | 2.2, 2.4, 2.5 | <!-- vk: --> |
| 6.5 | v1.0 release | Clean-machine QA against the zero-config success metric, cut and publish v1.0 | High | M | 3.5, 4.4, 5.6, 6.2, 6.3, 6.4 | <!-- vk: --> |

### Task Details

**6.1 - Name & branding**
- [ ] Name checked against existing Claude-usage tooling and macOS app namespace; bundle id reserved
- [ ] Tray icon (template image, dark/light) and app icon assets in place

**6.2 - Signing, notarization & release CI**
- [ ] Local signed build passes `spctl --assess`; notarization succeeds
- [ ] Pushing a version tag produces a published GitHub Release with a notarized .dmg
- [ ] Secrets (signing cert, notarization creds) stored as repo secrets, documented for forks

**6.3 - Docs & license**
- [ ] README covers: what it does, install, the exact settings.json modifications (verbatim), uninstall, privacy posture (loopback-only, local-only data)
- [ ] Architecture doc explains the OTel-primary/transcript-backfill design and the session→cwd join
- [ ] LICENSE committed; contribution guide covers dev setup for both Rust and Svelte sides

**6.4 - Onboarding polish & hardening**
- [ ] Error sweep: unreadable settings.json, no transcripts dir, full disk, DB locked, port stolen mid-run — all degrade with actionable messages
- [ ] Onboarding copy reviewed; every destructive/file-touching step has explicit consent
- [ ] Health view covers all failure states discovered in the sweep

**6.5 - v1.0 release**
- [ ] Clean-machine test: install → onboard → data flowing in <2 minutes with zero manual file edits
- [ ] Backfill diff report shows <1% capture gap over a week of dogfooding
- [ ] v1.0 tagged, released, install verified from the published .dmg

---

## Dependencies

- Claude Code OTel surface: `CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_LOGS_EXPORTER`, `claude_code.api_request` event schema (undocumented; version-tolerant parsing required)
- Claude Code settings/hooks surface: `env` block, `SessionStart` hook contract (`session_id`, `cwd` on stdin)
- Transcript JSONL format and `~/.claude/projects/` layout (30-day default retention)
- LiteLLM pricing JSON (pinned URL) for the remote pricing refresh
- Apple Developer account for signing/notarization

## Out of Scope

- Configurable persistence layers (Postgres/Mongo) — SQLite only
- Multi-machine sync, team aggregation
- Windows/Linux (post-V1 candidate)
- Billing reconciliation; alerting/budgets (post-V1 candidate)
- Tracking non-Claude-Code usage

## Open Questions

- [x] 3.1 spike outcome: exact dedup key (blocks 3.4 design detail, not Epic 1–2 work) — resolved: exact `request_id` (see `docs/notes/dedup-key.md`)
- [ ] Final project name (6.1)

## Related Documents

| Document | Purpose | Status |
|----------|---------|--------|
| docs/prd.md | Product Requirements | Current |

---

## Changelog

- **2026-06-11**: Initial development plan created from PRD (post-review revision: Svelte, App Support DB location, bundled+remote pricing, dedup spike, pause semantics, port-conflict and restart-session handling)
