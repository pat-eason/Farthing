# Pricing table (task 3.3)

How backfilled rows get an API-equivalent `cost_usd` (transcripts store no cost; OTel rows carry their own and are never re-priced).

## Layering

`src-tauri/src/pricing.rs`, three layers, all merged per-model (later wins):

1. **Bundled** — `src-tauri/data/pricing-bundled.json`, compiled in via `include_str!`. Always available; the app prices known models with zero network.
2. **Local cache** — `pricing-cache.json` in the app data dir, written by the last successful remote refresh. Overlaid at load (synchronous, network-free).
3. **Remote refresh** — one GET against the pinned LiteLLM URL, spawned at app start (`tauri::async_runtime::spawn`), 10s whole-request timeout, fail-silent. The payload must parse as a JSON object containing ≥1 well-formed Anthropic `claude-*` entry before it replaces the cache file (atomic write-then-rename) or touches the in-memory table; any failure leaves both untouched.

Pinned URL: `https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`

## Bundled snapshot provenance

Generated from LiteLLM `model_prices_and_context_window.json` at commit `3b40ac987fb4fe08061b67dda91b286dc41bee28` (2026-06-10), filtered to `litellm_provider == "anthropic"` + key starts with `claude`, cost fields only. 22 models; covers every model observed in the local transcript corpus (`claude-opus-4-8`, `claude-opus-4-7`, `claude-haiku-4-5-20251001`, `claude-fable-5`, `claude-sonnet-4-6`) plus the claude-3/4/4.5 generations. Versioning lives in the file's `_claude_usage_tracker` entry (`schema`, `snapshot_commit`, `snapshot_date`).

To regenerate: fetch the URL above, apply the same filter keeping `litellm_provider`, `input_cost_per_token`, `output_cost_per_token`, `cache_read_input_token_cost`, `cache_creation_input_token_cost`, `cache_creation_input_token_cost_above_1hr`, and update the `_claude_usage_tracker` provenance entry. The file is prettier-ignored (byte-exact generator output).

## Cost formula

```
cost = input × input_rate
     + output × output_rate
     + cache_read × cache_read_rate            (≈ 0.1× input)
     + cache_write_5m × write_5m_rate          (1.25× input)
     + cache_write_1h × write_1h_rate          (2× input)
```

- Rates come from the LiteLLM entry's explicit fields; when a field is missing the multiplier fallback (0.1× / 1.25× / 2×) is applied to that entry's input rate. The bundled snapshot's explicit fields sit exactly on the published multipliers (asserted in tests).
- Cache-creation tokens without a 5m/1h split (OTel rows, older transcripts) price at the **5m rate** — Claude Code's default TTL. With a split, any unsplit remainder also prices at 5m.

## Model-name lookup

Tried in order: exact match → provider prefix stripped (`anthropic/...`) → date suffix stripped (`claude-sonnet-4-5-20250929` → `claude-sonnet-4-5`) → newest dated variant of a dateless name (`claude-3-haiku` → `claude-3-haiku-20240307`).

## Unknown models (tokens-only contract)

`PricingTable::cost_for` returns `CostOutcome::UnknownModel` (`usd() == None`) for models the table doesn't know, `<synthetic>`, or a missing model. The backfill engine (3.4) stores such rows with `cost_usd = NULL`; **a `source = 'backfill'` row with `NULL` cost is the tokens-only flag** the UI keys off (Epic 4/5) — costs are never guessed. OTel rows always carry their exporter-computed `cost_usd`, so they are unaffected by table staleness.

## Live verification

`cargo run --example refresh_pricing` exercises the production refresh path against the real URL (TLS via rustls): fetch → validate → cache write → reload. Verified 2026-06-11: 22 models fetched, cache reloaded as `bundled+cache`.
