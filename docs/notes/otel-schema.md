# Claude Code OTel schema notes (task 1.6 e2e verification)

Findings from running real headless Claude Code sessions (`claude -p`,
v2.1.173, 2026-06-11) against the production receiver stack. Feeds the
dedup-identity spike (3.1) and the settings.json installer (2.1).

## Verification setup

- Receiver: `cargo run --example e2e_receiver -- <data-dir>` — the exact
  production `Db::open_in_dir` + `receiver::run` stack on `127.0.0.1:43177`,
  headless (no Tauri shell). DB inspected with `sqlite3`.
- Env vars set inline on the `claude` process only; `~/.claude/settings.json`
  untouched.
- SessionStart hook supplied via `claude --settings <file>` (not the global
  settings file):
  `curl -s -m 2 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:43177/session`
- 5 sessions run; 3 with a working export config produced 4 `api_request`
  rows total.

## Env var matrix (critical for task 2.1)

Claude Code v2.1.173 only honors the **signal-specific** OTLP exporter vars
for logs. Verified by permutation:

| Run | Env block | `requests` rows |
|-----|-----------|-----------------|
| 1 | `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` + `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:43177` (generic only) | 0 |
| 2 | generic pair + `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL=http/json` | 0 |
| 3 | generic pair + `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` + `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://127.0.0.1:43177/v1/logs` | 1 (correct) |
| 5 | signal-specific only (no generic vars at all) | 1 (correct) |

All runs also set `CLAUDE_CODE_ENABLE_TELEMETRY=1` and
`OTEL_LOGS_EXPORTER=otlp`. Notable: even the generic *endpoint* is ignored
when only `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` is signal-specific (run 2) —
the exporter needs **both** signal-specific vars. The minimal working block
the installer should write is exactly 4 keys:

```json
{
  "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
  "OTEL_LOGS_EXPORTER": "otlp",
  "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL": "http/json",
  "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "http://127.0.0.1:43177/v1/logs"
}
```

Note the signal-specific endpoint is used **as-is** (it must include the
`/v1/logs` path), unlike the generic endpoint which gets the path appended
per the OTLP spec.

**Task 2.1 implementation note**: the merge engine
(`src/settings_merge.rs`, `APP_ENV`) writes these 4 keys plus the generic
`OTEL_EXPORTER_OTLP_PROTOCOL=http/json` (5 total, per the plan's acceptance
criterion) as belt-and-suspenders for Claude Code versions that fall back to
the generic protocol var. The generic `OTEL_EXPORTER_OTLP_ENDPOINT` is
deliberately **not** written so the app never redirects other OTel signals a
user may export elsewhere.

## Row-vs-transcript reconciliation

Transcript files at
`~/.claude/projects/-Users-dev-Projects-farthing/<session_id>.jsonl`
(read-only).

**Run 3** (single-request turn, session `0082329c…`): 1 API request → 1 row.

| Field | OTel row | Transcript `message.usage` |
|-------|----------|---------------------------|
| request_id | `req_011CbwwEqCGUpoEXkE8kW2hA` | `requestId` identical |
| input_tokens | 2 | 2 |
| output_tokens | 88 | 88 |
| cache_read_tokens | 45322 | 45322 |
| cache_creation_tokens | 8215 | 8215 |
| model | claude-fable-5 | claude-fable-5 |
| cost_usd | 0.1524295 | n/a (matches CLI `total_cost_usd` exactly) |

**Run 4** (tool-use turn, session `cb17380f…`): 2 API requests → 2 rows.
Both rows reconcile exactly per `request_id` (input 8217/2, output 103/17,
cache_read 22800/45346, cache_creation 22546/8332), and
`SUM(cost_usd) = 0.773896` equals the CLI `total_cost_usd` to the digit.

**Sessions**: all 5 sessions got a `sessions` row with the correct `cwd`
(`/Users/dev/Projects/farthing`), `source='hook'`, via the
manually-configured SessionStart hook. Zero ingest failures across all runs.

## Request-identity findings (feeds spike 3.1)

- `api_request` carries `request_id` (`req_…`) that matches the transcript
  `requestId` **exactly** — verified on 3 requests across 2 sessions. Exact-id
  dedup looks viable; 3.1 should confirm across ≥3 sessions per its criteria
  (2 down) and decide collision/fallback behavior.
- **Transcript lines are not 1:1 with API requests**: one streamed API
  response can emit multiple `assistant` lines sharing the same `requestId`
  and identical `usage` (run 4: 3 assistant lines, 2 distinct requestIds).
  Backfill (3.2/3.4) must dedup transcript lines by `requestId` or it will
  double-count.
- Timestamps: OTel `timeUnixNano` lands 47-106ms after the request's **last**
  transcript line, but up to ~900ms after its first line (streaming writes
  the first assistant line before completion). A ts-window fallback key would
  need ≥1s tolerance; exact-id makes this moot when present.
- Other identity-ish attributes on `api_request`: `prompt.id`,
  `event.sequence`, `session.id`, plus account identity (`user.id`,
  `user.email`, `organization.id`, …) which we deliberately do not store.

## Event schema observations (confirms task 1.4 capture)

- Event name: `event.name` attribute unprefixed (`"api_request"`);
  `body.stringValue` prefixed (`"claude_code.api_request"`).
- `intValue` arrives as both JSON number and string within one batch;
  `timeUnixNano`/`observedTimeUnixNano` are nanosecond strings.
- Full `api_request` attribute set (v2.1.173): `session.id`, `event.name`,
  `event.timestamp`, `event.sequence`, `prompt.id`, `request_id`, `model`,
  `input_tokens`, `output_tokens`, `cache_read_tokens`,
  `cache_creation_tokens`, `cost_usd` (double), `cost_usd_micros` (int),
  `duration_ms`, `speed`, `query_source` (`"sdk"` for `-p` runs),
  `terminal.type`, and account identity attributes.
- **No cache TTL split on the OTel event**: only the `cache_creation_tokens`
  total. The 5m/1h ephemeral breakdown exists solely in the transcript
  (`message.usage.cache_creation.ephemeral_{5m,1h}_input_tokens`), so the
  nullable `cache_creation_5m/1h_tokens` columns can only be filled by
  backfill (3.2), as designed.
- `cost_usd` on the OTel event matches the CLI-reported session cost exactly;
  live rows need no local pricing computation.
- A session that makes no API request still POSTs an empty `{}` body.

## Gaps

- `api_error` was not observed live (no API failures occurred during the e2e
  runs; they are not cheaply inducible — auth/endpoint failures in `-p` mode
  abort before the exporter flushes). The
  `tests/fixtures/otlp_logs_api_error.json` fixture remains reconstructed,
  not captured. Replace opportunistically when a real one shows up.
- Only `query_source="sdk"` observed (`-p` mode); interactive-session values
  not yet sampled.
