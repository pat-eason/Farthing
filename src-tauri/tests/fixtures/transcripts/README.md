# Transcript fixtures

Sanitized real Claude Code transcripts for `src/transcript.rs` (task 3.2).
Every line is **derived from a real transcript** under `~/.claude/projects/`:
structure, key order, ids, timestamps, models, `cwd`, `isSidechain`, and the
full `message.usage` objects are verbatim; all prompt/response content is
replaced with `"[redacted]"` (assistant/user `message.content`, tool
results, attachments collapsed to their `type`, any other string > 80 chars).

## Files

- `main-session.jsonl` — first 25 lines of a real main-session transcript
  (Claude Code v2.1.173, 2026-06-11, sonnet). Covers 8 non-assistant line
  types (`last-prompt`, `mode`, `permission-mode`, `attachment`,
  `file-history-snapshot`, `user`, `ai-title`) plus 6 `assistant` lines
  forming 2 streaming groups (one `requestId` spanning 2 and 4 lines with
  byte-identical usage — the normal streaming shape).
- `sidechain.jsonl` — first 8 lines of a real subagent transcript
  (`<session>/subagents/agent-….jsonl`, v2.1.154). 4 `assistant` lines, all
  `isSidechain: true`, forming 2 groups whose `output_tokens` _grow_ between
  lines (3 → 136); exercises the 5m cache-creation split
  (`ephemeral_5m_input_tokens` non-zero, `1h` zero — the inverse of the main
  fixture).
- `edge-cases.jsonl` — real corpus oddities the 3.1 spike identified
  (`docs/notes/dedup-key.md`), assembled from two transcripts:
  - lines 1–6: the cumulative-growth group (`req_011CbqsNS9RVtSnLhZqXW4md`):
    one requestId, 6 lines, `output_tokens` grows 5 → 1004, same
    `message.id`; collapse must take the last line
  - lines 7–9: a request whose final line is a `model: "<synthetic>"`
    all-zero line **carrying the same requestId** as the 2 real lines before
    it; the synthetic line must not clobber the real usage
  - line 10: a requestId-less `<synthetic>` line ("No response requested."),
    all-zero usage; skipped entirely by collapse
  - line 11: an unknown future line type (`quantum-checkpoint`,
    hand-written) — must be skipped silently
  - line 12: a real assistant line truncated mid-JSON (hand-truncated) —
    counted malformed, not fatal

## Regenerating

Fixtures were produced by a one-shot script that reads the source
transcripts read-only and applies the redaction rules above. Source files
(local to Pat's machine, not in the repo):

- `-Users-dev-Projects-acme-app/5e6aa3df-f340-46ad-8c40-d613f7073b97.jsonl` (main)
- `-Users-dev-Projects-project2/56594d25-94a4-4449-9a7a-a3c654b5c4a3/subagents/agent-acb24f2158e2fb8a9.jsonl` (sidechain)
- `-Users-dev-Projects-acme-api/9cbb992d-413b-4132-9d1a-1cadc00b0f0f.jsonl` (edge groups)
- `-Users-dev-Projects-project2/e6437f12-a18e-4372-a5ad-502c07250e2a.jsonl` (synthetic line)
