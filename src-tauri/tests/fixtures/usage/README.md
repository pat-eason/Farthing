# Usage-limits fixtures

Test payloads for the `usage_limits.rs` parsing tests.

## Provenance

Reverse-engineered from the ccstatusline dist bundle and a live API call on
2026-06-12. See `docs/notes/subscription-usage.md` for full provenance and the
reference ccstatusline cache shape.

The endpoint is `GET https://api.anthropic.com/api/oauth/usage` with header
`anthropic-beta: oauth-2025-04-20`. The token is the Claude Code keychain
credential (`security find-generic-password -s "Claude Code-credentials" -w`),
field `claudeAiOauth.accessToken`.

## Files

- `full_response.json` — complete response with all known fields, realistic
  values taken from an actual Claude Max session (low utilization, extra_usage
  disabled). This is the canonical happy-path fixture.
- `null_buckets.json` — all bucket fields present but `utilization` and
  `resets_at` are `null`. Tests version-tolerant parsing: the parser must not
  panic or error when optional fields are absent.
- `unknown_fields.json` — full response with extra unknown fields sprinkled at
  both the bucket and top-level scope. Tests that the parser ignores unknown
  keys on schema drift (forward-compatibility requirement from CLAUDE.md).
  Also contains `seven_day.resets_at` as epoch-seconds (`1750197600` =
  2026-06-17T20:00:00Z) to exercise the integer-vs-ISO-string normalisation path
  alongside an ISO string in the same document.
- `epoch_resets_at.json` — all `resets_at` values are epoch-seconds integers.
  Tests the normalisation path in isolation (no ISO strings present).

## Schema observations

- `resets_at` appears as either an ISO-8601 string or a bare epoch-seconds
  integer in the same API. Parsers must accept both and normalise to UTC
  `DateTime`.
- `extra_usage` can be `null` at the top level (not just the inner fields).
- Unknown top-level keys (e.g. future quota buckets) must be ignored, not
  rejected.
- `monthly_limit` inside `extra_usage` can be `null` even when the object itself
  is present.
