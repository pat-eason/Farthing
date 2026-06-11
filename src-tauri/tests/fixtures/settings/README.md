# settings.json fixtures

Test inputs for the settings.json merge engine (`src/settings_merge.rs`,
task 2.1). The merge engine is the highest-blast-radius code in the app
(it rewrites the user's `~/.claude/settings.json`), so every fixture here
exercises a real-world shape the engine must never corrupt.

## Provenance

- `realworld.json` — anonymized copy of a real, heavily-customized
  `~/.claude/settings.json` (Claude Code v2.1.173, 2026-06-11): status line,
  plugins, marketplaces, a large `spinnerVerbs` block, and assorted top-level
  flags. A local marketplace directory path was replaced with a placeholder;
  structure and key order are otherwise verbatim. Notably has **no** `env`
  or `hooks` blocks (the common case).
- `preexisting_env.json` — hand-built: an `env` block that mixes a
  non-telemetry user var (`MY_CUSTOM_VAR`), an app-owned key already at the
  app value (`CLAUDE_CODE_ENABLE_TELEMETRY`), an app-owned key at a
  **different** value (`OTEL_LOGS_EXPORTER=console`), and foreign OTel vars
  the app does not own (`OTEL_EXPORTER_OTLP_ENDPOINT`,
  `OTEL_METRICS_EXPORTER`). Drives conflict-detection tests.
- `preexisting_hooks.json` — hand-built: a user `SessionStart` hook group
  plus an unrelated `PostToolUse` group, matching the documented Claude Code
  hooks schema (matcher group → inner `hooks` array of
  `{"type": "command", ...}`).
- `malformed.json` — truncated JSON (unclosed objects). The engine must
  abort and never write when it cannot parse.

Missing-file and empty-file cases are constructed in tests with `tempfile`
rather than checked-in fixtures.
