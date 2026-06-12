# Claude Usage Tracker (working name)

A macOS menu bar app that makes Claude Code token and dollar usage visible with **zero manual configuration**. Install it, click "Set up" once, and every Claude Code session on your machine starts reporting per-request usage: cost, tokens (input/output/cache read/cache creation), model, project, and session, surfaced through a menu bar popover and a full desktop UI.

> The name is a working name; a rename (working default: **BarTab**) is planned before the public v1.0 release. See [docs/notes/naming.md](docs/notes/naming.md).

## What it does

Claude Code already records everything needed to answer "what am I spending?": it can export per-request `claude_code.api_request` telemetry events over OTLP, and it writes per-message usage into session transcripts under `~/.claude/projects/`. Consuming either today requires manual OTel wiring or point-in-time CLI tools.

This app closes that gap:

- **Embedded OTLP receiver**: a loopback-only HTTP server on `127.0.0.1:43177` receives Claude Code's telemetry events live and stores one row per API request in a local SQLite database.
- **Self-installing configuration**: onboarding shows you an exact diff of the `~/.claude/settings.json` changes it needs (see [the verbatim changes](#exactly-what-it-changes-in-settingsjson) below), takes a timestamped backup, and applies them only after you confirm.
- **Transcript backfill**: on first run it parses your existing session transcripts (up to ~30 days of history by default retention), so charts are populated from day one. Every later start runs an incremental pass that recovers anything exported while the app was not running. Live and backfilled rows are deduplicated by `request_id`, so nothing double-counts.
- **Menu bar popover**: today's cost, token breakdown, session count, top projects, and a 7/30-day cost sparkline, updating live as events arrive.
- **Desktop UI**: cost over time, sessions (with per-session drill-in), tokens & cache analysis, and per-project rollups, all facetable by project, model, date range, and main-vs-subagent traffic.
- **Health view**: receiver status, config state, ingest counters, backfill progress, and a diff report comparing live capture against transcript ground truth.

Costs are **API-equivalent spend**: computed the way the API would bill those tokens. If you are on a subscription plan, treat the dollar figures as notional.

## Install

1. Download the latest `.dmg` from the GitHub Releases page.
2. Open it and drag the app to Applications.
3. Launch the app, then click the menu bar icon. First run opens onboarding:
   - It shows the exact `settings.json` diff and any conflicts with existing OTel configuration. Nothing is written until you confirm.
   - It registers the app as a login item (LaunchAgent) so the receiver is always up. This is shown and toggleable in Settings.
4. **Restart any running Claude Code sessions.** The `env` block is read at session startup, so sessions started before setup never export.

That's it: new Claude Code sessions report usage automatically, and the first backfill pass fills in your existing history.

To build from source instead, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Exactly what it changes in settings.json

Onboarding deep-merges the following into `~/.claude/settings.json` (and nothing else). This is the verbatim result of applying the merge to an empty file; with an existing file, every other byte is preserved and these keys are added:

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
    "OTEL_LOGS_EXPORTER": "otlp",
    "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL": "http/json",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "http://127.0.0.1:43177/v1/logs"
  },
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -m 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:43177/session >/dev/null 2>&1 || true"
          }
        ]
      }
    ]
  }
}
```

What each piece is for:

- The five `env` keys turn on Claude Code's built-in telemetry and point its **logs** exporter at the app's loopback receiver. The signal-specific pair (`OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` / `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`) is what Claude Code actually honors (verified by permutation in [docs/notes/otel-schema.md](docs/notes/otel-schema.md)); the generic `OTEL_EXPORTER_OTLP_PROTOCOL` is tolerance for versions that read the generic var. The generic `OTEL_EXPORTER_OTLP_ENDPOINT` is deliberately **not** set, so other OTel signals you may export elsewhere are never redirected.
- The `SessionStart` hook POSTs the hook's stdin JSON (`session_id` + `cwd`) to the receiver, which is how usage rows get attributed to a project directory (OTel events don't carry `cwd`). The command is fail-silent with a 2-second timeout: if the app isn't running, the hook exits 0 immediately and Claude Code is never slowed down or shown an error.

Safety properties of the merge (all fixture-tested; see `src-tauri/src/settings_merge.rs`):

- A timestamped backup of `settings.json` is written before any change; backups live in the app's data directory and are **never** deleted, not even by uninstall.
- The write is atomic (temp file + rename) and aborts entirely if the existing file is malformed or has an unexpected shape; the app never writes a partial or lossy result.
- Conflict detection: if you already have OTel env vars pointing elsewhere, onboarding shows the conflict and requires an explicit choice; it never silently overwrites.
- Port `43177` is fixed (chosen to avoid the standard OTLP `4317`/`4318`). If something else holds the port, the app reports it in the Health view rather than rebinding (the endpoint in `settings.json` is literal).

## Uninstall

Settings → Uninstall reverses everything, with a confirmation screen listing exactly what will and won't be removed:

1. **settings.json**: a strict unmerge removes only app-owned content: each of the five env keys (only if still holding the exact value the app wrote; values you edited are left alone) and any `SessionStart` hook whose command targets `http://127.0.0.1:43177/session`. One last backup is taken first. Everything else in the file is untouched.
2. **LaunchAgent**: the login item is removed.
3. **Database**: deleted only if you explicitly opt in; otherwise your usage history stays on disk.

Not removed: the settings.json backups (kept so you can restore any earlier state) and the app bundle itself (drag it to the Trash).

## Privacy posture

- **Loopback only**: the receiver binds `127.0.0.1` exclusively. Nothing listens on a network interface; no data ever leaves your machine.
- **Local-only data**: all usage data lives in a SQLite database at `~/Library/Application Support/com.peason.farthing/usage.db`. There is no telemetry, no analytics, no remote sync of any kind.
- **No content stored**: only usage numbers and metadata (tokens, cost, model, session id, timestamps, project directory paths). Prompt and response content is never stored. Account-identity attributes present on Claude Code's OTel events (`user.email`, `user.id`, `organization.id`, ...) are deliberately not persisted.
- **One outbound request, optional in effect**: on startup the app refreshes its model-pricing table from a pinned LiteLLM pricing JSON URL (fail-silent, cached locally, used only to price backfilled rows for which Claude Code did not report a cost). No usage data is sent; it is a plain GET for a public file. If it fails or is blocked, the bundled pricing snapshot is used.

## Architecture

See [docs/architecture.md](docs/architecture.md) for the full design: the OTel-primary/transcript-backfill dual-source model, the session→cwd join, dedup, pricing, storage schema, and process layout.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup (Rust + Svelte), checks, test instructions, and repo conventions. Releases are tag-driven; [docs/release.md](docs/release.md) documents the signing/notarization pipeline.

## License

[MIT](LICENSE).
