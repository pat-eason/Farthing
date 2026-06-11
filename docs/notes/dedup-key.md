# Dedup key decision (spike 3.1)

**Decision: dedup on exact `request_id`.** The OTel `api_request` attribute
`request_id` and the transcript `requestId` are the same string (`req_…`),
verified field-for-field on every request observed. Fuzzy
(session_id + ts-window + token-signature) matching is demoted to a
fallback for rows missing the id, which in practice is only synthetic
transcript lines that represent no API usage at all.

## Evidence A: side-by-side OTel vs transcript (4 sessions, 6 requests)

Two sources, all on Claude Code v2.1.173:

- **Task 1.6 e2e runs** (2026-06-11, `claude -p`, sonnet): sessions
  `0082329c…` (1 request) and `cb17380f…` (2 requests, tool use). Full
  reconciliation tables in `docs/notes/otel-schema.md`.
- **Spike 3.1 controlled runs** (2026-06-11, `claude -p`, haiku): production
  receiver stack (`cargo run --example e2e_receiver`) behind a tee proxy that
  captured the raw OTLP JSON bodies before forwarding, so the comparison
  below is against the actual exported payloads, not just parsed DB rows.
  Transcripts at `~/.claude/projects/-private-tmp-spike31-cwd/`.

| Session | Request | OTel `request_id` | Transcript `requestId` | in/out/cache_read/cache_create (OTel) | same (transcript) |
|---|---|---|---|---|---|
| `7fe6ea9e…` | 1 of 1 | `req_011Cbx2FnW9ouCvj4k4xJ3v9` | identical | 10 / 46 / 22251 / 22188 | identical |
| `8557a34b…` (tool use) | 1 of 2 | `req_011Cbx2GnJXd9i2c1K38YCZX` | identical | 10 / 124 / 22251 / 22199 | identical |
| `8557a34b…` | 2 of 2 | `req_011Cbx2GyuHLcfnyTAmEVMFc` | identical | 6 / 12 / 22251 / 22115 | identical |

`session.id` on the OTel event equals the transcript `sessionId` / filename
in all cases. OTel `event.timestamp` landed 39–55ms after the request's last
transcript line (1.6 saw 47–106ms), consistent and small.

## Evidence B: transcript corpus analysis (481 files)

Read-only scan of all local transcripts under `~/.claude/projects/`
(134,809 lines; 54,642 `assistant` lines, every one carrying
`message.usage`):

- **26,815 distinct `requestId` values; zero appear in more than one
  transcript file.** No cross-session collisions in the corpus.
- **Only 35 assistant lines lack `requestId`, and all 35 are
  `model: "<synthetic>"`** ("No response requested.") with all-zero usage.
  They represent no API traffic and must simply be skipped by backfill.
- **16,422 requestIds span multiple assistant lines** (streaming writes one
  line per content block). 16,416 of those groups have byte-identical
  `usage`; the 6 exceptions are:
  - a trailing all-zero usage line (`service_tier: null`,
    `inference_geo: null`) appended after the real lines; and
  - one cumulative-growth case (`req_011CbqsNS9RVtSnLhZqXW4md`): later
    lines gained an `iterations` array and `output_tokens` grew 5 → 1004
    while `message.id` stayed the same. The **last** non-zero line carries
    the final totals.
- 5 requestIds map to two `message.id` values (a UUID plus a `msg_…` id);
  `message.id` is therefore *not* a reliable identity. `requestId` is.

The cumulative-growth case is also why a token-signature key alone would be
wrong: two lines of the same request can disagree on `output_tokens` by 999.

## The key

- **Primary (exact) key: `request_id`**, stored verbatim from either source.
  Globally unique in practice (26,815/26,815); treat it as the dedup
  identity across `otel` and `backfill` rows.
- **Collision behavior**: a row insert (either source) that matches an
  existing `requests.request_id` is a duplicate of the same API request, not
  a collision; skip it. When both sources have the row, **the `otel` row
  wins and is never overwritten**: it carries the authoritative `cost_usd`
  straight from Claude Code, which transcripts lack. Backfill may UPDATE
  only the transcript-exclusive columns (`cache_creation_5m_tokens` /
  `cache_creation_1h_tokens`) on an existing otel row. Recommended
  enforcement for 3.4: `CREATE UNIQUE INDEX idx_requests_request_id ON
  requests (request_id) WHERE request_id IS NOT NULL` plus
  `INSERT … ON CONFLICT DO NOTHING` (or the partial-update variant above).
- **Fallback (fuzzy) key**, only for rows where `request_id` is NULL on
  either side: `(session_id, model, timestamp window ±2s, token signature
  input/output/cache_read/cache_creation)`. The ±2s window covers the
  observed 39–106ms OTel-after-transcript skew plus streaming's up-to-~900ms
  first-line-to-last-line spread. Expected to be exercised rarely if ever:
  every observed `api_request` event and every non-synthetic transcript line
  carries the id. Candidate real consumer: `api_error` rows, whose
  documented schema includes `request_id` but which we have not yet captured
  live (see `otel-schema.md` gaps); errors never appear in transcripts as
  usage lines, so they don't double-count regardless.

## Backfill implementation notes (feeds 3.2 / 3.4)

1. Collapse transcript lines per `requestId` **before** insert: take the
   last line with non-zero usage (equivalently, max `output_tokens`); ignore
   trailing all-zero lines.
2. Skip `model == "<synthetic>"` / missing-`requestId` lines entirely.
3. One transcript request → one `requests` row keyed by `request_id`,
   `source='backfill'`, cost computed from the pricing table (3.3).
4. Incremental passes (byte offsets) can re-see the head of a request's line
   group; the unique index makes re-inserts idempotent.
