# Development Plan: Claude Usage Tracker (working name)

> **Generated from:** docs/prd.md
> **Created:** 2026-06-11
> **Last synced:** 2026-06-12
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
| 1. Capture Pipeline | Complete | 100% |
| 2. Self-Install & Lifecycle | Complete | 100% |
| 3. Transcript Backfill | Complete | 100% |
| 4. Menu Bar UI | Complete | 100% |
| 5. Desktop UI | Complete | 100% |
| 6. OSS Release | In Progress | 70% |

---

## Epic 1: Capture Pipeline (COMPLETE)

Foundation: Tauri scaffold, the embedded OTLP receiver, event parsing, the SQLite schema, and the session→cwd mapping endpoint. Exit state: with manually-configured env vars, a real Claude Code session produces queryable rows in SQLite. No UI beyond a stub window.

### Acceptance Criteria

- [x] A real Claude Code session (manually configured to export OTLP) results in correct per-request rows in `usage.db`
- [x] Receiver binds `127.0.0.1:43177` only; non-loopback connections are rejected
- [x] `session_id → cwd` mappings are captured via `POST /session`
- [x] Unknown/extra fields in OTel payloads never crash ingestion (version-tolerant parsing)

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

## Epic 2: Self-Install & Lifecycle (COMPLETE)

The zero-config promise: safe settings.json merge/unmerge, onboarding with diff preview and conflict detection, LaunchAgent autostart, uninstall, and the health view. Highest-blast-radius epic — fixture tests are non-negotiable.

### Acceptance Criteria

- [ ] Fresh machine: install app → click through onboarding → restart Claude Code session → data flows, with zero manual file edits
- [x] settings.json modifications are previewed, backed up, strictly additive to app-owned keys, and fully reversible
- [x] Pre-existing OTel/telemetry config is detected and never silently overwritten
- [x] Health view accurately reports receiver, config, and data-flow state

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

## Epic 3: Transcript Backfill (COMPLETE)

Day-one history and gap recovery: JSONL parsing, pricing, dedup against live rows, and incremental offset-based recovery. Starts with the dedup-identity spike flagged in the PRD review.

### Acceptance Criteria

- [x] First launch populates all available transcript history (up to ~30 days) with computed costs
- [x] Live OTel rows and backfilled rows never double-count the same activity
- [x] Sessions missed while the app was down (including their cwd mappings) are recovered on next start
- [x] Backfill diff report quantifies capture completeness (<1% missing target)

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

## Epic 4: Menu Bar UI (COMPLETE)

The daily-driver surface: tray icon, popover with today's metrics, sparkline, live updates, pause/resume.

### Acceptance Criteria

- [x] Popover shows today's cost, token split, session count, 7/30-day sparkline, top 3 projects — rendering in <100ms
- [x] Values update near-real-time (~5s) as events arrive
- [x] No Dock icon in menu-bar-only mode
- [x] Pause discards incoming events (200 + drop) with a visible paused badge

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 4.1 | Tray icon & popover shell | Tauri v2 TrayIcon, popover window anchored to tray, `ActivationPolicy::Accessory`, menu (open app / pause / quit) | High | M | 1.1 | done <!-- vk: --> |
| 4.2 | Today-metrics queries & popover content | SQL aggregations (local-midnight day boundary, distinct session_ids), popover layout: cost, in/out/cache tokens, sessions, top 3 projects | High | M | 1.4, 4.1 | done <!-- vk: --> |
| 4.3 | Sparkline | 7/30-day cost sparkline in popover (uPlot/LayerCake) | Medium | S | 4.2 | done <!-- vk: --> |
| 4.4 | Live updates & pause/resume | Rust→frontend event push on ingest; pause state (receiver 200+discard), paused badge, resume | Medium | M | 4.2 | done <!-- vk: --> |

### Task Details

**4.1 - Tray icon & popover shell**
- [x] Tray icon visible; click opens popover positioned at the tray; click-away dismisses — live-verified with synthetic CGEvent clicks on the status item: popover shows anchored under the icon (screenshot-confirmed), a click elsewhere hides it, and a second tray click closes instead of reopening (300ms auto-hide suppression window)
- [x] No Dock icon while only the popover exists; Dock icon appears when the desktop window opens and disappears when it closes — `lsappinfo` ApplicationType observed as `UIElement` at startup and with the popover open, `Foreground` after the open-app menu item, back to `UIElement` after closing the window (close is intercepted: hide + policy flip, app stays resident)
- [x] Menu actions wired: open desktop app, pause/resume (stub ok until 4.4), quit — all three exercised via the real tray menu: open shows + focuses `main`, "Pause capture" check state toggles and mirrors into `TrayState::paused` (receiver hookup is 4.4), quit exits the process

**4.2 - Today-metrics queries & popover content**
- [x] Day = local midnight boundary; sessions = distinct session_ids active today (resume doesn't double-count) — `src-tauri/src/metrics.rs`: `local_day_window` computes `[today 00:00 local, tomorrow 00:00 local)` via `chrono::Local`, DST-correct (ambiguous midnight → earliest instant, skipped midnight → first existing local time); `metrics_for_window` counts `COUNT(DISTINCT session_id)` so a resumed session (same id) counts once. Unit-tested: inclusive-start/exclusive-end boundaries, 5-request resumed session counts as 1, NULL session_ids excluded, `api_error` rows count the session but not the request
- [x] Popover shows cost, input/output/cache-read/cache-creation tokens, session count, top 3 projects by cost — `today_metrics` command + `/popover` view: cost headline, 2×2 token grid, "N sessions · M requests", top-3 projects ranked by `SUM(cost_usd)` through `sessions.cwd` (NULL cwd / missing session rows collapse into one "(unknown project)" bucket; full path in the tooltip, last segment displayed). Screenshot-verified against a seeded DB (600 sessions, 15k requests today). Refreshes on window focus (every tray open) + 5s poll; 4.4 swaps the poll for ingest push
- [x] Renders in <100ms against a DB seeded with 100k+ rows — schema v3 adds the covering index `idx_requests_time_rollup` (replacing `idx_requests_timestamp`, its leftmost prefix), making both rollup queries index-only: warm release query on a 120k-row DB with an extreme 15k-request day went 58ms → 7.5ms. Live dev app against that DB (via new `CLAUDE_USAGE_TRACKER_DATA_DIR` override + `cargo run --example seed_metrics_db`): on-screen fetch+render instrumentation read **23.0ms** end-to-end (unoptimized dev build; release is faster). `metrics_query_under_100ms_with_150k_rows` pins the budget in CI
- [x] Cost labeled "API-equivalent" — label sits beside the cost headline (screenshot-verified); unpriced rows (unknown model) are excluded from the total and surfaced as "N requests with unknown pricing excluded from cost (tokens counted)" rather than silently counting as $0

**4.3 - Sparkline**
- [x] 7-day and 30-day toggle; bars/line match SQL aggregation values exactly — new `daily_costs(days)` command (`src-tauri/src/metrics.rs`): one bucket per trailing local calendar day (each boundary an independently-resolved local midnight, DST-correct), every bucket the same indexed range scan `today_metrics` uses; `daily_series_buckets_match_per_day_aggregation_exactly` asserts each bucket equals `metrics_for_window` for its window. Live (seeded 120k rows + real backfill): 7d view showed $13,329 / today bar $2,440 vs direct SQL $13,329.19 / $2,439.69; clicking 30d refetched and showed $17,237 vs SQL $17,237.15 (screenshot-verified, both ranges). Rendered as a dependency-free SVG bar chart (`src/lib/Sparkline.svelte`, today highlighted, per-day tooltips) instead of pulling in uPlot/LayerCake; fetch+render 54ms in dev
- [x] Renders correctly with sparse data (gaps, single day, empty) — gap days come back as explicit zero buckets from the backend (frontend never infers); zero days draw baseline only but keep a full-height tooltip hover target. Live-verified all three: dense run showed a mid-week gap day as empty space; a single-active-day DB rendered one bar in both 7d and 30d (total $4.50 exact); a fresh empty DB rendered flat baseline + "No cost in the last 7 days." with no errors. Unit tests pin gaps/single-day/empty/boundary-ms cases (158 total green)

**4.4 - Live updates & pause/resume**
- [x] New ingested event updates popover values within one export interval (~5s) without reopening — `/v1/logs` now reports its stored-row count (`ingest::ingest_logs` returns it) and fires an `ingest:stored` Tauri event (wired in `lib.rs`) only when rows actually landed; the popover replaces the 4.2 poll with a 200ms-debounced refetch on that event (focus refresh kept). Live-verified with the popover held open: posting a synthetic `api_request` flipped the display $0.00→$4.20 (and later $4.20→$5.50) within 1.5s of the POST, screenshot-confirmed, no reopen — well inside the ~5s export interval
- [x] Pause: receiver returns 200 and discards; tray shows paused badge; resume restores ingestion — shared `Arc<AtomicBool>` (`capture::CaptureState` → `IngestState::with_pause_flag`) checked per request: paused `/v1/logs` and `/session` return their success codes but write nothing and move no counters (malformed JSON still 400s; protocol unchanged). Live: paused POSTs got 200 with row counts frozen, tray showed a "Paused" title badge next to the icon, popover showed a "Capture paused" banner + Resume button; clicking Resume cleared flag/badge/banner and the next POST landed as a row + live popover update. Two real bugs found and fixed live: tray/menu mutations from the command thread need `run_on_main_thread`, and `set_title(None)` doesn't clear on macOS (cleared with `Some("")`)
- [x] Paused state persists across app restart — persisted in `meta` under `capture_paused`; `CaptureState::load` reads it before the receiver spawns and `tray::setup` seeds the menu check + badge from it. Live: paused → killed and relaunched the dev app → `meta` read back `1`, "Paused" badge restored, "Pause capture" check mark restored (AXMenuItemMarkChar `✓`), and a post-restart POST was discarded (200, row count unchanged). Unit-tested by reopening the database file as a fresh process would (165 tests green)

---

## Epic 5: Desktop UI (COMPLETE)

The analysis surface: faceted query layer and the four main views. Can develop against seeded fixture data in parallel with Epics 2–3.

### Acceptance Criteria

- [x] All four views (cost over time, sessions, tokens/cache, projects) functional with global facets: project, model, date range, query_source
- [x] Queries over ~1M rows return in <500ms
- [x] Cost consistently labeled API-equivalent; tokens-only rows (unknown pricing) handled visibly

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 5.1 | Desktop window shell & navigation | Full window from tray, view navigation, activation policy flip, dark/light | High | M | 4.1 | done <!-- vk: --> |
| 5.2 | Faceted query layer | Tauri commands wrapping SQL aggregations with shared facet params (project, model, date range, query_source); seed-data script for dev | High | M | 1.2 | done <!-- vk: --> |
| 5.3 | Cost-over-time view | Line/bar chart, day/week/month/all ranges, stack by model or project | High | M | 5.1, 5.2 | done <!-- vk: --> |
| 5.4 | Sessions view | Sortable table (cost, tokens, duration, project, models) + per-session detail drill-in | Medium | M | 5.1, 5.2 | done <!-- vk: --> |
| 5.5 | Tokens & cache view | In/out/cache-read/cache-creation over time; cache hit-rate trend | Medium | M | 5.1, 5.2 | done <!-- vk: --> |
| 5.6 | Projects view | Per-directory rollups (cost, tokens, sessions), cleaned path display | Medium | S | 5.1, 5.2 | done <!-- vk: --> |

### Task Details

**5.1 - Desktop window shell & navigation**
- [x] Window opens from tray menu and popover; closing returns to Accessory mode — tray-menu path existed since 4.1; new `open_main_window` command (`src-tauri/src/tray.rs`, main-thread-dispatched) backs an "Open Claude Usage Tracker" footer button in the popover (popover hidden explicitly on handoff). Live-verified with synthetic clicks + `lsappinfo`: tray menu → window shown, `ApplicationType` flipped `UIElement`→`Foreground`; close button → window hidden (0 AX windows), flipped back to `UIElement`, app resident; popover button → same Foreground flip with the popover dismissed. Main window now 1100x720 (min 800x560, centered)
- [x] Navigation between the four views preserves active facet selections — shell layout `src/routes/(app)/+layout.svelte` (sidebar: four views + Health/Settings, which moved into the group with URLs unchanged) around stub view pages (5.3-5.6 fill them in); shared facet state in `src/lib/facets.svelte.ts` (range/source/project/model, module-level `$state` so it survives navigation and window close/reopen) edited via `FacetBar.svelte`. "/" now auto-forwards to `/cost` when already configured (onboarding unchanged otherwise). Verified in-browser (facets set on /cost survived the full cost→sessions→tokens→projects→cost round trip) and in the live app (Source=Subagents only set on Cost persisted onto Sessions, Clear (1) shown)
- [x] Respects system dark/light appearance — all new shell/facet/stub styles carry `prefers-color-scheme: dark` variants like the existing pages. Verified via playwright `emulateMedia` (light/dark computed colors + screenshots) and live: the running window re-rendered correctly when system appearance was toggled light↔dark via AppleScript, no restart

**5.2 - Faceted query layer**
- [x] One shared facet struct (project, model, date range, query_source) applied across all aggregation commands — `Facets` (`src-tauri/src/queries.rs`) deserialized identically by all five new commands: `usage_summary`, `usage_series` (per-local-day buckets, optional model/project grouping for the 5.3 stacking toggle), `session_rollups` (sort/limit/offset pushed into SQL), `project_rollups` (cost-descending), `facet_options` (the bar's option lists); typed frontend wrappers + `toFacets` bridge from the 5.1 facet state in `src/lib/queries.ts`. Subagent filtering works off `query_source = 'subagent'`, which backfill now writes for sidechain lines (v4 migration resets ingest offsets once so the next pass heals pre-v4 rows)
- [x] Queries on a 1M-row seeded DB return <500ms (indexes verified with `EXPLAIN QUERY PLAN`) — schema v4 adds two covering indexes: time-leading `idx_requests_facet_rollup` (range scans) and session-leading `idx_requests_session_rollup` (index-ordered `GROUP BY session_id`, no sorter: 226ms vs 858ms for the month sessions rollup at 1M). Project facets compile to `session_id IN (SELECT …)` subqueries, never per-request joins (a joined month summary measured 1.9s; subquery 122ms). `cargo run --release --example seed_metrics_db -- <dir> 1000000` gates every shape and passed: worst warm query 419ms (month series grouped by project), all others ≤268ms. EXPLAIN QUERY PLAN tests pin covering-index use and reject TEMP B-TREE grouping
- [x] Seed script generates realistic multi-project/multi-model/multi-week fixture data — `seed_metrics_db` now spreads rows over 75 days across 13 projects (plus both unknown-project flavors: NULL-cwd sessions and orphan session ids), 4 models, a main/subagent/user/NULL `query_source` mix, transcript-style rows carrying the 5m/1h split, ~1/500 unpriced and ~1/400 `api_error` rows
- [x] Unit tests assert aggregation correctness against hand-computed fixtures — a 6-row hand-computed fixture drives exact-total assertions for every command and facet (including conjunctive combinations, custom-range `[start, end)` boundaries, main+subagent partitioning, and unknown-project = NULL cwd + missing session row); cross-checks force series buckets ≡ per-window summaries and project rollups ≡ per-project summaries so the views can't disagree; serde tests pin the frontend payload shapes; 199 tests green

**5.3 - Cost-over-time view**
- [x] Range presets (day/week/month/all) and custom range work; stacking toggles between model and project — `/cost` view (`src/routes/(app)/cost/+page.svelte`): the ungrouped `usage_series` is always the bar skeleton (explicit zero buckets) and a Total / By model / By project toggle paints stack segments from the grouped series (top 8 keys by cost colored, rest folded into "Other", null key = the unknown model/project bucket), so toggling stacking never changes bar heights. `FacetBar` gained the custom range (two date inputs seeded with the trailing week; inclusive dates → `[start, end)` in `toFacets`) plus real project/model datalists from `facet_options` (with an "(unknown project)" sentinel option). Browser-verified against a 150k-row seeded DB through the new `examples/query_bridge.rs` (production `queries::*_for` over localhost HTTP + a `__TAURI_INTERNALS__.invoke` shim, so the rendered data is the real command path): day/week/month/all rendered 1/7/30/76 buckets, custom 2026-05-20→05-29 rendered 10, both stackings rendered with stable legend/color order
- [x] Chart totals reconcile exactly with the sessions/projects views for the same facets — header totals come from the same `usage_summary` those views reconcile against, and a DEV-only footnote prints summary/series/grouped sums to 6 decimals. Browser-verified exact equality summary ≡ Σseries ≡ Σgrouped ≡ Σ`session_rollups` ≡ Σ`project_rollups` (cost and request counts) for month-all ($13656.684000, 149625 requests), a custom window ($1610.589000, 17694), week+subagent+model ($336.580200) and project=unknown ($590.510600, 132 sessions); the queries.rs cross-check tests pin the same invariants in Rust. Doing this surfaced a 5.2 seed bug, fixed here: `seed_metrics_db`'s spread multiplier (37) was far smaller than its modulus, silently collapsing the claimed 75-day spread onto ~2 days
- [x] Empty-state and sparse-data render sensibly — `requests == 0 && errors == 0` swaps the chart for a "No usage in this range" card (verified with a 2020 custom window; a backend-less browser shows the error message state instead). Sparse data: zero buckets draw baseline only but keep full-height tooltip targets (`src/lib/StackedBarChart.svelte`, same contract as the popover sparkline); a custom window straddling the seed's first day rendered 22 buckets with 9 bars and "$0.00" tooltips on the empty days. Light/dark screenshot-verified; cost is labeled API-equivalent with the unpriced-requests footnote carried over from the popover

**5.4 - Sessions view**
- [x] Table sortable by cost, tokens, duration, start time; facets apply — `/sessions` view (`src/routes/(app)/sessions/+page.svelte`) renders `session_rollups` pages (sort/direction/offset pushed into SQL, 100 rows per "Load more" page); clicking Start/Duration/Tokens/Cost toggles key then direction (`aria-sort` exposed). Browser-verified through `query_bridge` on a 150k-row seed: all four sort keys (plus cost ascending) matched an independent Python/sqlite3 rollup exactly (top-5 rows: cost $22.63/$21.89/$21.64/$21.56/$21.51, duration 715h29m leaders, start-desc May 16 13:38/13:30/05:57); header/dev-reconcile totals matched SQL to the cent ($6,355.2008 / 700 sessions unfaceted; $592.767 / 700 for month+sonnet+subagent); a facet combination with no rows (opus+subagent, legitimately 0 in the seed) rendered the empty-state card, not an error
- [x] Session detail shows per-request timeline, model mix, cache behavior, source tags — new `session_detail` command (`src-tauri/src/queries.rs`): same `Facets` as the table (so the drill-in always reconciles with the clicked row), per-request timeline capped at 1000 rows (timestamp-ascending; `total_rows` and the per-model mix always cover everything), model mix cost-descending, every row carrying both tags (`query_source` chip: main/subagent/user/…; data-source chip: otel/backfill) plus the 5m/1h cache-creation split. Drill-in is an inline expansion with model-mix + cache-behavior panels (hit rate = cache read / (cache read + input), definition printed in the UI) and the timeline table (error rows highlighted with the `error` chip, unpriced cost labeled). Browser-verified against SQL: seed-sess-478 under month+sonnet+subagent showed 23 rows/$1.16/1.2M tok in both the row and the mix (SQL: 23/$1.1592/1,169,136), cache split 50.8k/16.9k (SQL: 50,820/16,950), mixed otel+backfill tags, and an `api_error` timeline row (0 tokens, "—" cost). 5 new rust tests pin fixture rows, facet application, the cap, and the serde shape (204 total green); light/dark screenshot-verified
- [x] Sessions with no cwd mapping display as "unknown project," not errors — `SessionRollup.cwd`/`SessionDetail.cwd` stay NULL through the query layer (unit test covers NULL-cwd sessions, orphan session ids, and fully unknown ids returning empty data, never an error); the table renders them via `projectName(null)` = "(unknown project)" (italicized) and the drill-in header does the same. Browser-verified: seed's NULL-cwd sessions and `seed-orphan-27` (no sessions row at all) both listed, sorted, and drilled into normally

**5.5 - Tokens & cache view**
- [x] Four token series charted over time with same range controls as 5.3 — `/tokens` view (`src/routes/(app)/tokens/+page.svelte`): four small-multiple `StackedBarChart`s (Input/Output/Cache read/Cache creation) over one ungrouped `usage_series` fetch, headline totals from the same `usage_summary` the other views reconcile against; the shared FacetBar supplies the identical range controls (day/week/month/all/custom) and facets. Browser-verified through `query_bridge` on a 150k-row seed: presets rendered 1/7/30/76 buckets, custom 2026-05-20→05-29 rendered 10; summary ≡ Σseries for all four counters in every window, and the month window matched an independent Python/sqlite3 scan exactly (in 15,303,327 / out 76,674,527 / cr 2,733,118,527 / cc 278,830,527); 2020 custom window rendered the empty-state card; light/dark screenshot-verified
- [x] Cache hit-rate trend (cache_read / (cache_read + input)) defined in UI copy and matches SQL — dedicated trend chart with the definition printed beneath it ("Cache hit rate = cache read ÷ (cache read + input) tokens: the share of prompt tokens served from cache"); days with no prompt tokens draw no bar (rate null, not zero) and the header carries the overall rate. Browser-verified against SQL: overall 99.4432% unfaceted month (SQL: 99.4432%), per-day tooltips 99.5%/99.4% for Jun 10/11 (SQL: 99.5/99.4), custom window 99.4430% (SQL: 99.4430%), and month+sonnet+subagent 99.4431% with split 26,054,941/8,689,320 (SQL identical), so the trend holds under facets too
- [x] 5m vs 1h cache-creation split visible where backfill data provides it — `SeriesPoint` now carries `cache_creation_5m_tokens`/`cache_creation_1h_tokens` (same NULL-when-absent semantics as `usage_summary`; both v4 covering indexes already include the columns so every scan stays index-only, and grouped series carry the split per key). The cache-creation chart stacks 5m TTL / 1h TTL / "unsplit (live capture)" per day with split totals in the card legend, falling back to a "split unavailable: transcript-backfilled data only" note when no matching row carries it. Tooltip-verified vs SQL (Jun 10: 5m 613.4k / 1h 204.6k / unsplit 5.0M ≡ 613,447/204,583/5,004,461); 2 new rust tests pin per-bucket split ≡ per-window summary split and the serde shape (206 total green)

**5.6 - Projects view**
- [x] Directories rolled up with cost, tokens, session counts; sorted by cost — `/projects` view (`src/routes/(app)/projects/+page.svelte`): one `project_rollups` fetch (SQL `ORDER BY cost DESC`, already index-only from 5.2) renders project name, cleaned path, sessions, requests, total tokens (full breakdown in the tooltip), cost (unpriced `~` chip), and a share-of-cost bar/percent; headline reconciles against the same `usage_summary` as the other views. Browser-verified through `query_bridge` on a 150k-row seed: all 13 rows (12 projects + the "(unknown project)" bucket, which legitimately tops the cost sort at $590.5106/132 sessions/6,556 requests/291,603,068 tokens) matched an independent Python/sqlite3 rollup exactly, page Σ ≡ summary ($6358.063800 / 700 sessions), empty-state card on a 2020 custom window, light/dark screenshot-verified
- [x] Paths displayed cleaned (`~/...`); click-through applies that project as a global facet — new `home_dir` command (`src-tauri/src/queries.rs`, `app.path().home_dir()`; bridge maps it to `$HOME`) feeds `cleanPath` in `src/lib/format.ts` (home prefix → `~`, non-home and unknown-home paths pass through, absolute path kept in the hover title); verified with the bridge run under `HOME=/Users/dev` so every seed path rendered `~/Projects/…`. Clicking a row sets the shared 5.1 project facet (`UNKNOWN_PROJECT_OPTION` for the NULL-cwd bucket; clicking the active "filtered" row clears it): verified frontend-app click → FacetBar "Clear (1)" + table refetched to that single row ($496.451200/48 sessions) and /sessions inherited the filter (48 sessions, all frontend-app); unknown-project click reconciled at $590.510600/132. 206 rust tests green (home_dir contract pinned in the mock-runtime command test); the now-unused `PlannedView.svelte` stub is deleted

---

## Epic 6: OSS Release (IN PROGRESS)

Public-readiness: naming, signing/notarization, CI releases, docs, final hardening, and the v1.0 cut.

### Acceptance Criteria

- [ ] Notarized .dmg installs and passes Gatekeeper on a clean machine
- [ ] Tagged release builds, signs, notarizes, and publishes via GitHub Actions
- [x] README/docs sufficient for a stranger to install, trust the settings.json behavior, and contribute

### Tasks

| ID | Title | Description | Priority | Complexity | Depends On | Status |
|----|-------|-------------|----------|------------|------------|--------|
| 6.1 | Name & branding | Final name (avoid ccusage/CCSeva collision), icon set, bundle identifier | Medium | S | — | done <!-- vk: --> |
| 6.2 | Signing, notarization & release CI | Developer ID signing, notarization, GitHub Actions tag→.dmg release pipeline | High | L | 1.1 | done <!-- vk: --> |
| 6.3 | Docs & license | README (incl. exact settings.json changes made), architecture doc, contribution guide, LICENSE | Medium | M | 6.1 | done <!-- vk: --> |
| 6.4 | Onboarding polish & hardening | First-run UX pass, error/edge-case sweep (locked files, permission denials, odd configs), copy review | Medium | M | 2.2, 2.4, 2.5 | done <!-- vk: --> |
| 6.5 | v1.0 release | Clean-machine QA against the zero-config success metric, cut and publish v1.0 | High | M | 3.5, 4.4, 5.6, 6.2, 6.3, 6.4 | <!-- vk: --> |

### Task Details

**6.1 - Name & branding**
- [x] Name checked against existing Claude-usage tooling and macOS app namespace; bundle id reserved — `docs/notes/naming.md`: 9 candidates checked (2026-06-11) against npm/crates.io/Homebrew/PyPI/Mac App Store/GitHub/web; the niche is crowded (the working name itself collides with a 2,690★ Swift menu bar app; metermaid.app and burnbar are taken by direct competitors). **Working default: BarTab** (`com.peason.bartab`; npm/crates/brew/MAS all free; only collisions are dead or hospitality-POS). Identifier deliberately NOT applied yet: final name needs human confirmation, and the identifier change moves `app_data_dir` (migration steps in the doc's rename checklist; land with 6.2/6.3)
- [x] Tray icon (template image, dark/light) and app icon assets in place — placeholder set from `scripts/generate-icons.py` (PIL, deterministic): `art/app-icon.png` 1024px master regenerated into `src-tauri/icons/` via `pnpm tauri icon`, plus `src-tauri/icons/tray-icon.png` 32px black+alpha glyph wired in `tray.rs` via `include_bytes!` + `Image::from_bytes` (tauri `image-png` feature) with `icon_as_template(true)` so macOS recolors it for dark/light menu bars; decode pinned by a unit test (207 total green)

**6.2 - Signing, notarization & release CI**
- [ ] Local signed build passes `spctl --assess`; notarization succeeds — **blocked on human: no Apple Developer credentials exist yet** (see `docs/release.md` "Status / blockers"). Everything automatable is in place: `bundle.macOS.hardenedRuntime: true` + `minimumSystemVersion: 10.15` in `tauri.conf.json` (notarization requires the hardened runtime), local signed-build + `codesign`/`stapler`/`spctl` verification steps documented in `docs/release.md`. Unsigned local build verified: `pnpm tauri build --bundles app` produces the `.app` (ad-hoc linker signature; `spctl` rejects it as expected for an un-notarized build)
- [ ] Pushing a version tag produces a published GitHub Release with a notarized .dmg — pipeline built and `actionlint`-clean, end-to-end run **blocked on the same Apple credentials**: `.github/workflows/release.yml` triggers on `v*` tags (+ `workflow_dispatch` dry runs), verifies tag == `tauri.conf.json` version, builds a universal (arm64+x86_64) `.dmg`, signs/notarizes via Tauri's native `APPLE_*` env support when secrets exist, hard-verifies with `codesign --verify --deep --strict` + `xcrun stapler validate` + `spctl --assess --type exec` so an un-assessed build can't publish, uploads `.dmg` + SHA-256 checksums, and creates the GitHub Release (published only when notarized; signed-only/unsigned builds become drafts with warning notes)
- [x] Secrets (signing cert, notarization creds) stored as repo secrets, documented for forks — `docs/release.md` documents all six secrets (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`) with exact steps to produce each, the App Store Connect API-key alternative, and the fork story (no secrets → same pipeline, unsigned draft). The workflow gates each step on secret presence via job-level `HAS_SIGNING`/`HAS_NOTARY` flags, so the secrets need only be added (no workflow edits). Storing the actual secret values is the human-blocked half

**6.3 - Docs & license**
- [x] README covers: what it does, install, the exact settings.json modifications (verbatim), uninstall, privacy posture (loopback-only, local-only data) — `README.md` rewritten: feature overview, .dmg install steps (incl. the restart-running-sessions caveat), uninstall will/won't list mirroring `uninstall.rs`, and a privacy section (loopback-only bind, local-only SQLite, no content/identity attributes stored, the one pricing-refresh GET disclosed). The settings.json section quotes the merge output **verbatim**: generated by running `apply_merge` on an empty map and pasting the exact JSON (5 env keys + fail-silent `SessionStart` curl hook), with per-key rationale and the merge-engine safety properties (backup, atomic write, abort-on-malformed, conflict gating, fixed port)
- [x] Architecture doc explains the OTel-primary/transcript-backfill design and the session→cwd join — `docs/architecture.md`: system diagram, source-comparison table motivating OTel-primary (authoritative `cost_usd`, live) vs transcript-backfill (history / gap recovery / 5m-1h enrichment), `request_id` dedup with otel-wins conflict rules (linking `docs/notes/dedup-key.md` evidence), and a dedicated session→cwd section covering all three mechanisms (SessionStart hook upsert, backfill self-heal, query-time LEFT JOIN with "(unknown project)" fallback). Also: settings-merge ownership rules, schema/index overview (v1–v4), pricing layering, UI/event flow, reliability posture, module map
- [x] LICENSE committed; contribution guide covers dev setup for both Rust and Svelte sides — `LICENSE` (MIT, per PRD; `license` fields added to `src-tauri/Cargo.toml` and `package.json`) and `CONTRIBUTING.md`: prerequisites, `pnpm tauri dev`/build, the two sandbox env overrides (`CLAUDE_USAGE_TRACKER_DATA_DIR`/`_SETTINGS_PATH`) with a fully-sandboxed dev recipe, every CI gate as local commands (prettier/eslint/svelte-check + fmt/clippy/`cargo test`), the 7 headless dev examples, fixture/test conventions (never write to `~/.claude`, append-only migrations, version-tolerant parsing), and PR expectations. All four docs cross-linked; full check suite green over the new markdown

**6.4 - Onboarding polish & hardening**
- [x] Error sweep: unreadable settings.json, no transcripts dir, full disk, DB locked, port stolen mid-run — all degrade with actionable messages — new `settings_merge::describe_settings_error` (names the file + remediation per kind: `chmod u+rw` for PermissionDenied, free-space for StorageFull, fix-JSON/restore-backup for Malformed; unit + real-chmod-000 tests) is wired into every onboarding/uninstall/health settings read; a missing `~/.claude/projects` is an empty backfill pass plus an explicit fresh-machine note (new `TranscriptsInfo`); full-disk/locked-DB store failures keep the receiver alive and record `IngestStats.last_failure` detail (drop-table storage-failure test), and a failed stored-events read degrades `health_status` to since-launch counters with a `db_error` message instead of erroring the view; a receiver serve-loop that returns (the port-stolen/server-died class) now always flips status to `Failed` ("Relaunch this app") instead of silently claiming Listening. Unreadable + malformed settings.json verified live in the real onboarding UI (chmod 000 → actionable error screen → fix → "Try again" → preview)
- [x] Onboarding copy reviewed; every destructive/file-touching step has explicit consent — full pass over onboarding/settings/health copy: settings.json merge is gated on the user-confirmed verbatim diff, conflict overwrite is a separate explicit choice, uninstall is gated on the will/won't preview with the DB deletion opt-in unchecked by default, and the one previously-undisclosed file-touching step (best-effort LaunchAgent registration during apply) is now stated on the preview screen before consent ("Applying also registers the app to start at login… nothing else on your system is touched"); error screens state explicitly that settings.json was not modified
- [x] Health view covers all failure states discovered in the sweep — `/health` now renders `db_error` (degraded-totals banner), the most recent ingest failure detail, the missing-transcripts fresh-machine note, the capture-paused warn box with a working Resume button, and a `capture_paused` no-events cause that trumps the ambiguous pair (the old `paused` cause kind renamed `idle`). All 9 states browser-verified through the production `compute_health`/`compute_status` via the new `examples/health_bridge.rs` scenario bridge (healthy, fresh_machine, db_locked, disk_full, port_conflict, receiver_failed, paused→resume, config_unreadable, config_conflicting); 212 tests green

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
- [ ] Final project name (6.1) — working default **BarTab** picked with collision evidence in `docs/notes/naming.md`; awaiting human confirmation before the rename lands (checklist in the doc)

## Related Documents

| Document | Purpose | Status |
|----------|---------|--------|
| docs/prd.md | Product Requirements | Current |

---

## Changelog

- **2026-06-11**: Initial development plan created from PRD (post-review revision: Svelte, App Support DB location, bundled+remote pricing, dedup spike, pause semantics, port-conflict and restart-session handling)
