# PRD: Claude Usage Tracker (working name)

> **Author:** Pat Eason
> **Created:** 2026-06-11
> **Status:** Draft

## Problem Statement

Claude Code users have no good visibility into their token and dollar usage. Claude Code records everything needed (per-request token counts via OTel events, per-message usage in session transcripts), but consuming that data today requires manual setup: wiring OTel env vars to a collector, standing up a metrics backend, or running CLI tools like `ccusage` against transcript files. None of the existing options provide a zero-config, always-on macOS experience with both at-a-glance daily metrics and deep faceted analysis (by project, model, session, cache behavior).

This project ships a single Tauri macOS app that acts as a local OTLP collector for Claude Code's built-in telemetry, owns its own SQLite store, self-installs the required Claude Code configuration, and surfaces usage through a menu bar popover and a full desktop UI.

Key insight from design: Claude Code hooks do **not** carry token/cost data (verified against current docs); the data sources are (1) OTel `claude_code.api_request` log events, which carry per-request `cost_usd` + all four token counts, and (2) session transcript JSONL files, which carry per-message usage + `cwd` + `sessionId`. The app uses OTel as the live pipeline and transcripts as backfill/recovery.

## Goals

1. **Zero-config capture:** install the app → Claude Code usage data flows automatically. No manual env vars, no shell profile edits, no separate collector.
2. **Faceted visibility:** cost over time, cost per session, tokens in/out, cache usage, and session counts — all facetable by project/directory and model.
3. **Day-one value:** charts populate immediately from existing transcript history (up to 30 days) via backfill, not from an empty database.
4. **Open-source quality:** signed/notarized releases, clean install/uninstall, documented architecture, safe handling of the user's `~/.claude/settings.json`.

## Non-Goals (Out of Scope)

- **Configurable persistence layers** (Postgres, MongoDB). SQLite only; single-writer local workload. A thin storage module keeps the door open, but no alternative backends ship.
- **Multi-machine sync or team aggregation.** Single-machine, local-only data.
- **Windows/Linux support** for the MVP. macOS first (menu bar UX is macOS-specific); cross-platform is a possible later milestone since Tauri supports it.
- **Real billing reconciliation.** Computed `cost_usd` is API-equivalent spend; subscription users see notional cost, clearly labeled as such.
- **Tracking non-Claude-Code usage** (raw API, other agents/IDEs).
- **Alerting/budgets** (e.g., "warn me at $X/day") — candidate for post-V1.

## Target Users / Personas

### Persona 1: Heavy Claude Code power user (primary)
- **Description:** Engineer running many Claude Code sessions daily across multiple repos, often with subagents and background tasks.
- **Needs:** Glanceable daily spend in the menu bar; the ability to answer "which project/model is burning tokens?", "how effective is my cache usage?", "how many sessions this week?"
- **Pain points:** Usage data is invisible without manual OTel/collector setup; existing CLI tools are point-in-time and unfaceted; no ambient awareness of spend.

### Persona 2: Open-source adopter
- **Description:** Any macOS Claude Code user who finds the project on GitHub.
- **Needs:** Download a notarized .dmg, open it, click "Set up" — and trust that the app won't corrupt their existing `~/.claude/settings.json` or conflict with an existing OTel setup.
- **Pain points:** Wary of tools that mutate dotfiles; needs transparent show-what-will-change onboarding and a clean uninstall.

## Functional Requirements

### FR-1: Embedded OTLP receiver (live pipeline)
- App runs a localhost-only HTTP server (axum) on a fixed non-standard port (default `43177`, avoiding collision with standard OTLP `4317/4318`).
- Accepts OTLP `http/json` on `POST /v1/logs` and `POST /v1/metrics`.
- Ingests `claude_code.api_request` log events as the row-level source of truth: `cost_usd`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `model`, `query_source`, `session.id`, timestamp.
- `claude_code.api_error` events ingested for completeness (error counts surfaced in the desktop UI).
- Metrics endpoint accepts and discards (or no-ops) — aggregations are derived in SQL from events; the installer does not enable the metrics exporter.
- Binds to `127.0.0.1` only; rejects non-loopback connections.

### FR-2: Self-installing Claude Code configuration
- First-run onboarding detects current state of `~/.claude/settings.json` and shows an explicit diff of proposed changes before writing.
- Non-destructive deep-merge into `settings.json`, with a timestamped backup written first:
  - `env` block: `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`, `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:43177`.
  - `SessionStart` hook entry that POSTs hook stdin (contains `session_id` + `cwd`) to `http://127.0.0.1:43177/session` (curl with short timeout, fail-silent).
- Conflict detection: if the user already has OTel env vars or telemetry config pointing elsewhere, surface it and require an explicit choice — never silently overwrite.
- Onboarding explicitly tells the user to restart any running Claude Code sessions: the `env` block is read at session startup, so pre-existing sessions never export.
- Port conflict at boot: never auto-rebind (settings.json holds the literal endpoint); surface "port 43177 in use" in the health view with remediation guidance.
- Registers the app as a login item (LaunchAgent via `tauri-plugin-autostart`) so the receiver is always up.
- Uninstall flow reverses the merge: removes only the keys/hook entries the app added, restores nothing else, removes the LaunchAgent, and optionally deletes the database.

### FR-3: Session → project mapping
- `POST /session` endpoint stores `session_id → cwd` mappings from the SessionStart hook.
- OTel events join to project via `session.id` at query time (OTel events do not carry `cwd` — verified gap).
- Mapping self-heals via backfill: transcripts carry both `sessionId` and `cwd`, so sessions whose hook POST was missed (app down) are repaired on the next backfill pass.
- Project displayed as a cleaned path (e.g., `~/Projects/acme/web-app`), grouped by directory.

### FR-4: SQLite persistence
- Database lives at `~/Library/Application Support/<app>/usage.db` (platform-correct, owned by the app's uninstall flow, survives `~/.claude` cleanup tooling).
- App creates and migrates `usage.db` on boot (rusqlite; WAL mode).
- Core tables (indicative): `requests` (per-API-request rows), `sessions` (session_id, cwd, first_seen, last_seen, source), `ingest_state` (transcript byte offsets for idempotent backfill), `meta` (schema version).
- Dedup strategy so live OTel rows and backfilled transcript rows for the same activity don't double-count. **Requires a spike**: verify whether `claude_code.api_request` events carry a request identifier matching the transcript's `requestId`; if yes, dedup on it — if no, fall back to (session_id, timestamp window, token signature) fuzzy matching.
- All UI metrics are SQL aggregations over `requests` joined to `sessions`.

### FR-5: Transcript backfill & gap recovery (MVP)
- On first run: parse all JSONL transcripts under `~/.claude/projects/<encoded-cwd>/*.jsonl`, extracting per-assistant-message `message.usage` (input, output, cache_read, cache_creation incl. 5m/1h breakdown), `model`, `sessionId`, `cwd`, `timestamp`, `isSidechain`.
- Cost for backfilled rows computed from a per-model pricing table (transcripts do not store cost — verified). Pricing is a bundled, versioned data file plus an optional fail-silent remote refresh on app start (pinned LiteLLM pricing JSON URL, cached locally) so new models price correctly without an app update; unknown models display tokens-only.
- On every app start (and on a manual "Backfill now" action): incremental pass using stored byte offsets to recover anything exported while the app was down.
- Backfilled vs live rows are tagged by source for debuggability.

### FR-6: Menu bar UI
- Tauri v2 TrayIcon with popover window; `ActivationPolicy::Accessory` (no Dock icon while in menu-bar-only mode).
- Popover shows: today's cost, today's tokens (in/out/cache split), today's session count, sparkline of cost over the last 7/30 days, top 3 projects by today's cost.
- Live-updates as events arrive (logs export interval is ~5s, so near-real-time).
- Menu actions: open desktop app, pause/resume capture, quit. Pause = receiver keeps returning 200 but discards events (badge shows paused); the paused window is recoverable later via transcript backfill.

### FR-7: Desktop UI
- Full window opened from the tray (same Tauri app; activation policy flips to Regular while open).
- Views:
  - **Cost over time** — line/bar chart, selectable range (day/week/month/all), stacked by model or project.
  - **Sessions** — table of sessions with cost, tokens, duration, project, model(s); sortable; per-session detail.
  - **Tokens** — input vs output vs cache-read vs cache-creation over time; cache hit-rate trend.
  - **Projects** — per-directory rollups.
- Global facets applied across all views: project/directory, model, date range, query_source (main/subagent).
- Clear labeling that cost is API-equivalent (notional for subscription users).
- Metric definitions: day boundaries are local midnight; "# of sessions" = distinct `session_id`s active that day (a resumed session keeps its id and does not count twice).

### FR-8: Health & diagnostics
- Status indicator: receiver listening, last event received at, settings.json config state (installed/missing/conflicting), backfill progress.
- Detects "telemetry configured but no events arriving" and surfaces likely causes (Claude Code session predates config, port conflict).

## Non-Functional Requirements

- **Performance:** receiver ingestion and SQLite writes must be negligible (<1% CPU steady-state); menu bar popover renders in <100ms; desktop queries over a year of data (<1M rows realistic upper bound) return in <500ms with proper indexes.
- **Footprint:** Tauri keeps the app well under typical Electron weight; idle memory target <100MB.
- **Reliability:** app must never block or slow Claude Code — the hook POST is fail-silent with a 2s timeout; OTLP export failures are absorbed by Claude Code's exporter, and gaps are recovered by backfill. SQLite in WAL mode survives crashes.
- **Security/Privacy:** receiver binds loopback only; no data leaves the machine; no prompt/response content stored (only usage numbers + metadata); settings.json modifications are previewed, backed up, and reversible. OTel events include `user.email`/`user.id` attributes — stored locally only.
- **Distribution:** signed and notarized .dmg via GitHub Releases; CI builds; semver.
- **Open source:** MIT (or similar) license, README with architecture doc, contribution guide.

## Tech Stack

- **App shell:** Tauri v2 (Rust backend, webview frontend), macOS first.
- **Receiver:** axum HTTP server in the Tauri Rust process; serde for OTLP `http/json` decoding (no protobuf dependency).
- **Storage:** SQLite via rusqlite (WAL mode) in `~/Library/Application Support/<app>/`; migrations embedded in the binary.
- **Frontend:** TypeScript + Svelte with a lightweight chart lib (uPlot or LayerCake).
- **Autostart:** `tauri-plugin-autostart` (LaunchAgent).
- **CI/CD:** GitHub Actions — build, sign, notarize, release.

## Success Metrics

| Metric | Target | How Measured |
|--------|--------|--------------|
| Zero-config setup works | Fresh install → data flowing in <2 min with no manual file edits | Manual QA on a clean machine; onboarding telemetry-free (self-reported) |
| Capture completeness | <1% of API requests missing vs transcript ground truth over a week | Backfill diff report comparing OTel rows to transcript rows |
| Day-one history | First launch shows ≥ available transcript history (up to 30 days) | First-run backfill row counts |
| settings.json safety | Zero reported corruptions; merge is reversible | Unit tests on merge/unmerge against real-world settings fixtures; issue tracker |
| Performance | Idle CPU <1%, idle RAM <100MB, popover <100ms | Instruments profiling |
| OSS traction (post-launch) | GitHub stars / installs trending; issues triaged within a week | GitHub insights |

## Dependencies

- **Claude Code OTel surface:** `CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_LOGS_EXPORTER`, `claude_code.api_request` event schema (fields: `cost_usd`, token counts, `model`, `session.id`, `query_source`). Undocumented stability — schema may drift between Claude Code releases.
- **Claude Code settings/hooks surface:** `env` block in `settings.json`, `SessionStart` hook contract (`session_id`, `cwd` on stdin).
- **Transcript JSONL format:** `message.usage` shape, `sessionId`/`cwd`/`timestamp` fields, `~/.claude/projects/` layout, 30-day default retention (`cleanupPeriodDays`).
- **Model pricing data** for backfill cost computation (bundled table; needs updating as models ship).
- **Apple Developer account** for signing/notarization.

## Risks & Open Questions

| Risk/Question | Impact | Mitigation/Answer |
|---------------|--------|-------------------|
| Claude Code OTel event schema changes between releases | High | Version-tolerant parsing (ignore unknown fields, alert on missing required fields); health view surfaces ingest failures; transcripts as independent fallback source |
| settings.json merge corrupts user config | High | Preview diff before write, timestamped backup, strict deep-merge touching only app-owned keys, extensive fixture tests, reversible uninstall |
| Data dropped while app not running | Medium | LaunchAgent autostart + incremental transcript backfill on every start; dedup prevents double-count |
| Double-counting between OTel and backfill rows | Medium | Deterministic dedup keys (session_id + timestamp window + token signature); tag rows by source; backfill diff report |
| User already runs an OTel collector / has telemetry env set | Medium | Conflict detection in onboarding; fixed non-standard port (43177); explicit user choice, never silent overwrite |
| Pricing table staleness (backfill cost wrong for new models) | Low | Bundled table + fail-silent remote refresh (pinned LiteLLM URL); "unknown model" rows flagged with tokens-only display; OTel rows unaffected (cost_usd comes from Claude Code) |
| `cost_usd` semantics for subscription users | Low | Label as "API-equivalent spend" throughout the UI |
| Project name | Low | "farthing" is a working name; pick a real name before public release (avoid collision with ccusage/CCSeva) |
| Tokens-in attribution: per-request `input_tokens` includes full conversation prefix; cache fields dominate | Low | Surface cache_read separately and define "tokens in" precisely in UI copy/docs |

## Timeline / Milestones

No calendar deadlines; milestones define sequencing and demo-able cut points.

| Milestone | Description |
|-----------|-------------|
| M1: Pipeline | Receiver + SQLite + self-install merge + SessionStart mapping hook. Verifiable via SQL queries; no UI. |
| M2: Backfill | Transcript parser, pricing table, dedup, incremental gap recovery. First-run history populated. |
| M3: Menu bar | Tray popover with today's metrics + sparkline. Daily-driver usable. |
| M4: Desktop | Full UI: cost over time, sessions, tokens/cache, projects, faceting. |
| MVP = M1–M4 | Personal daily-driver complete. |
| V1.0 | OSS release: signing/notarization, CI releases, onboarding polish, conflict handling, uninstall flow, README/docs, real name. |
