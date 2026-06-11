# OTLP fixtures

Test payloads for the ingest pipeline (`src/ingest.rs`).

## Provenance

- `otlp_logs_api_request.json` — **captured from a real Claude Code session**
  (v2.1.173, 2026-06-11) by pointing `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` at a
  local dump server and running `claude -p`. Identity attributes
  (`user.email`, `work_email`, `user.id`, `organization.id`,
  `user.account_uuid`, `user.account_id`) were replaced with placeholders;
  everything else (structure, key order, value encodings) is verbatim. The
  batch contains 12 log records: one `api_request` plus `hook_execution_*` and
  `mcp_server_connection` events, which doubles as the unknown-event-tolerance
  fixture.
- `otlp_logs_api_error.json` — reconstructed, not captured. `api_error` events
  could not be triggered cheaply during development (auth/endpoint failures in
  `-p` mode abort before the exporter flushes). The envelope and identity
  attributes are copied from the real capture; the event attributes follow the
  documented `claude_code.api_error` schema (`error`, `model`, `status_code`,
  `duration_ms`, `attempt`, `request_id`). Replace with a real capture when
  task 1.6 (end-to-end verification) produces one.

## Schema observations from the real capture (feeds task 1.6 / 3.1)

- Event name lives in **`event.name` without the `claude_code.` prefix**
  (`"api_request"`); `body.stringValue` carries the prefixed form
  (`"claude_code.api_request"`).
- `intValue` is encoded **both** as a JSON number (`41`) and as a string
  (`"248"`) within a single export batch. Parsers must accept both.
- `timeUnixNano` / `observedTimeUnixNano` are strings of nanoseconds; an
  `event.timestamp` attribute (RFC3339) duplicates them.
- `api_request` carries `request_id` (`req_…`) — candidate exact dedup key for
  the transcript backfill spike (3.1).
- Other attributes seen on `api_request`: `prompt.id`, `event.sequence`,
  `cost_usd` (double), `cost_usd_micros` (int), `duration_ms`, `speed`,
  `query_source` (value `"sdk"` for a `-p` run), `terminal.type`.
- Claude Code only honors the **signal-specific** exporter vars: exports were
  protobuf until `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL=http/json` was set, and no
  exports happened without `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`. The settings
  installer (task 2.1) must set the signal-specific pair.
- A session that makes no API request still exports an **empty `{}` body** to
  `/v1/logs`.
