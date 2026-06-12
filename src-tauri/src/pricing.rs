//! Per-model pricing table and cost computation (task 3.3).
//!
//! Transcripts don't store cost (verified, PRD FR-4), so backfilled rows
//! (task 3.4) compute an API-equivalent `cost_usd` from token counts and a
//! per-model price list. Three layers, cheapest-staleness-first:
//!
//! 1. **Bundled**: `data/pricing-bundled.json`, a versioned snapshot of the
//!    LiteLLM community price list filtered to Anthropic `claude-*` models,
//!    compiled into the binary (`include_str!`). Always available.
//! 2. **Local cache**: `pricing-cache.json` in the app data dir, written by
//!    the last successful remote refresh. Overlaid on the bundled table at
//!    load; per-model, cache wins.
//! 3. **Remote refresh**: a fail-silent fetch of the pinned LiteLLM URL
//!    ([`PRICING_REFRESH_URL`]) spawned at app start — never awaited on the
//!    startup path, bounded by [`FETCH_TIMEOUT`], and the payload is
//!    schema-validated before it replaces the cache or touches the
//!    in-memory table. Any failure leaves the current table untouched.
//!
//! Unknown models are *flagged, not guessed*: [`PricingTable::cost_for`]
//! returns [`CostOutcome::UnknownModel`], the backfill engine stores the row
//! with `cost_usd = NULL`, and the UI renders such rows tokens-only (a
//! backfill row with NULL cost *is* the tokens-only flag; OTel rows always
//! carry their own `cost_usd`).
//!
//! # Cache write/read multipliers
//!
//! LiteLLM entries carry explicit per-token cache costs which match
//! Anthropic's published multipliers: cache read ≈ 0.1× input, 5-minute-TTL
//! cache write 1.25× input, 1-hour-TTL cache write 2× input. When an entry
//! omits a cache field the multiplier fallback is applied to its input
//! cost. Cache-creation tokens without a 5m/1h split (older transcripts,
//! OTel rows) price at the 5m rate — Claude Code's default TTL.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::AssistantUsage;

/// Pinned remote price list: LiteLLM's community-maintained
/// `model_prices_and_context_window.json` on `main`. The same source the
/// bundled snapshot was generated from.
pub const PRICING_REFRESH_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Cache file name inside the app data directory.
pub const CACHE_FILE_NAME: &str = "pricing-cache.json";

/// Whole-request budget for the remote refresh (connect + headers + body).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Cache file schema version; bump on incompatible changes.
const CACHE_SCHEMA: u32 = 1;

/// Multiplier fallbacks (vs `input_cost_per_token`) for LiteLLM entries
/// missing the explicit cache cost fields.
const CACHE_READ_MULTIPLIER: f64 = 0.1;
const CACHE_WRITE_5M_MULTIPLIER: f64 = 1.25;
const CACHE_WRITE_1H_MULTIPLIER: f64 = 2.0;

/// Bundled price snapshot (see module docs; provenance recorded in the
/// file's `_farthing` entry and `docs/notes/pricing.md`).
const BUNDLED_PRICING: &str = include_str!("../data/pricing-bundled.json");

/// Resolved per-token USD costs for one model. All fields are finite and
/// non-negative by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_cost_per_token: f64,
    /// 5-minute-TTL cache write; also the rate for unsplit cache-creation
    /// tokens (Claude Code's default TTL).
    pub cache_write_5m_cost_per_token: f64,
    /// 1-hour-TTL cache write.
    pub cache_write_1h_cost_per_token: f64,
}

impl ModelPricing {
    fn is_sane(&self) -> bool {
        [
            self.input_cost_per_token,
            self.output_cost_per_token,
            self.cache_read_cost_per_token,
            self.cache_write_5m_cost_per_token,
            self.cache_write_1h_cost_per_token,
        ]
        .iter()
        .all(|c| c.is_finite() && *c >= 0.0)
    }
}

/// Token counts to price. Mirrors the `requests` columns; build one from an
/// [`AssistantUsage`] via `From`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTokens {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// 5m/1h cache-creation split when the transcript carries it. When both
    /// are `None` the whole `cache_creation_tokens` prices at the 5m rate.
    pub cache_creation_5m_tokens: Option<i64>,
    pub cache_creation_1h_tokens: Option<i64>,
}

impl From<&AssistantUsage> for UsageTokens {
    fn from(u: &AssistantUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_creation_tokens: u.cache_creation_tokens,
            cache_creation_5m_tokens: u.cache_creation_5m_tokens,
            cache_creation_1h_tokens: u.cache_creation_1h_tokens,
        }
    }
}

/// Result of pricing one request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CostOutcome {
    /// API-equivalent cost in USD.
    Priced(f64),
    /// Model missing from the table (or no model at all). The caller stores
    /// `cost_usd = NULL`; the row surfaces tokens-only.
    UnknownModel,
}

impl CostOutcome {
    /// `Some(usd)` when priced, `None` for unknown models — shaped for the
    /// nullable `requests.cost_usd` column.
    pub fn usd(&self) -> Option<f64> {
        match self {
            CostOutcome::Priced(usd) => Some(*usd),
            CostOutcome::UnknownModel => None,
        }
    }
}

/// In-memory price table: bundled snapshot, optionally overlaid by the
/// local cache and/or a successful remote refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct PricingTable {
    models: HashMap<String, ModelPricing>,
    /// Provenance, for logs/diagnostics: `bundled`, `bundled+cache`,
    /// `bundled+remote`, …
    pub source: String,
}

impl PricingTable {
    /// Parse the compiled-in snapshot. Infallible in practice (the bundled
    /// file is covered by tests); a hypothetical parse failure yields an
    /// empty table rather than a panic, so the app still starts.
    pub fn bundled() -> Self {
        let models = serde_json::from_str::<Value>(BUNDLED_PRICING)
            .ok()
            .map(|v| parse_litellm(&v))
            .unwrap_or_default();
        if models.is_empty() {
            eprintln!("pricing: bundled table failed to parse; costs unavailable until refresh");
        }
        Self {
            models,
            source: "bundled".into(),
        }
    }

    /// Bundled table overlaid with the local cache file, when present and
    /// valid. Synchronous and network-free: safe on the startup path.
    pub fn load(data_dir: &Path) -> Self {
        let mut table = Self::bundled();
        match read_cache(&cache_path(data_dir)) {
            Ok(Some(models)) => {
                table.overlay(models);
                table.source = "bundled+cache".into();
            }
            Ok(None) => {}
            Err(reason) => {
                // Corrupt/foreign cache: ignore it. The next successful
                // refresh rewrites it.
                eprintln!("pricing: ignoring cache file: {reason}");
            }
        }
        table
    }

    /// Merge `models` over the current table (incoming entries win).
    fn overlay(&mut self, models: HashMap<String, ModelPricing>) {
        self.models.extend(models);
    }

    /// Find pricing for a model name as it appears in transcripts/OTel
    /// (`claude-opus-4-8`, `claude-haiku-4-5-20251001`) or with a provider
    /// prefix (`anthropic/claude-…`). Tries, in order: exact match, the
    /// date-suffix-stripped alias, and the newest dated variant of a
    /// dateless name.
    pub fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        let bare = model.trim().rsplit('/').next().unwrap_or(model).trim();
        if bare.is_empty() {
            return None;
        }
        if let Some(p) = self.models.get(bare) {
            return Some(p);
        }
        // "claude-sonnet-4-5-20250929" → "claude-sonnet-4-5"
        if let Some(stripped) = strip_date_suffix(bare) {
            if let Some(p) = self.models.get(stripped) {
                return Some(p);
            }
        }
        // "claude-3-haiku" → "claude-3-haiku-20240307" (newest dated key)
        self.models
            .iter()
            .filter(|(key, _)| {
                key.strip_prefix(bare)
                    .is_some_and(|rest| strip_date_suffix(key).is_some() && rest.len() == 9)
            })
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, pricing)| pricing)
    }

    /// Price one request. `None`/unknown model → [`CostOutcome::UnknownModel`].
    pub fn cost_for(&self, model: Option<&str>, tokens: &UsageTokens) -> CostOutcome {
        let Some(pricing) = model.and_then(|m| self.lookup(m)) else {
            return CostOutcome::UnknownModel;
        };
        let write_usd = match (
            tokens.cache_creation_5m_tokens,
            tokens.cache_creation_1h_tokens,
        ) {
            // No split (OTel rows, older transcripts): default 5m TTL.
            (None, None) => {
                tokens.cache_creation_tokens as f64 * pricing.cache_write_5m_cost_per_token
            }
            (five_m, one_h) => {
                let five_m = five_m.unwrap_or(0);
                let one_h = one_h.unwrap_or(0);
                // Any unsplit remainder prices at the default 5m rate.
                let rest = (tokens.cache_creation_tokens - five_m - one_h).max(0);
                (five_m + rest) as f64 * pricing.cache_write_5m_cost_per_token
                    + one_h as f64 * pricing.cache_write_1h_cost_per_token
            }
        };
        CostOutcome::Priced(
            tokens.input_tokens as f64 * pricing.input_cost_per_token
                + tokens.output_tokens as f64 * pricing.output_cost_per_token
                + tokens.cache_read_tokens as f64 * pricing.cache_read_cost_per_token
                + write_usd,
        )
    }

    /// Number of models in the table.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the table holds no models at all.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

/// Shared, refreshable pricing table, managed by Tauri.
#[derive(Clone)]
pub struct PricingState(pub Arc<RwLock<PricingTable>>);

impl PricingState {
    pub fn new(table: PricingTable) -> Self {
        Self(Arc::new(RwLock::new(table)))
    }

    /// Price one request against the current table.
    pub fn cost_for(&self, model: Option<&str>, tokens: &UsageTokens) -> CostOutcome {
        match self.0.read() {
            Ok(table) => table.cost_for(model, tokens),
            // Poisoned lock: a writer panicked. Treat as unknown rather
            // than propagate the panic.
            Err(_) => CostOutcome::UnknownModel,
        }
    }
}

/// Path of the local cache file inside the app data directory.
pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE_NAME)
}

/// Background refresh for app start: fetch the pinned URL, and on success
/// update both the cache file and the in-memory table. Completely
/// fail-silent — spawned, never awaited by the caller, and every error path
/// only logs.
pub async fn refresh(state: PricingState, data_dir: PathBuf) {
    match refresh_once(PRICING_REFRESH_URL, &state, &cache_path(&data_dir)).await {
        Ok(count) => eprintln!("pricing: refreshed {count} models from remote"),
        Err(reason) => eprintln!("pricing: remote refresh skipped: {reason}"),
    }
}

/// One refresh attempt against `url`. Validates the payload shape before
/// replacing the cache file or touching `state`; any failure leaves both
/// exactly as they were. Returns the number of models accepted.
pub async fn refresh_once(
    url: &str,
    state: &PricingState,
    cache_file: &Path,
) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("client init: {e}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("fetch: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("fetch: HTTP {}", response.status()));
    }
    let body = response.bytes().await.map_err(|e| format!("body: {e}"))?;

    // Schema validation: must be a JSON object containing at least one
    // well-formed Anthropic claude-* entry. Anything else is a drifted or
    // truncated payload; keep what we have.
    let value: Value =
        serde_json::from_slice(&body).map_err(|e| format!("payload is not JSON: {e}"))?;
    if !value.is_object() {
        return Err("payload is not a JSON object".into());
    }
    let models = parse_litellm(&value);
    if models.is_empty() {
        return Err("payload contains no recognizable claude model pricing".into());
    }

    write_cache(cache_file, &models).map_err(|e| format!("cache write: {e}"))?;

    let count = models.len();
    if let Ok(mut table) = state.0.write() {
        table.overlay(models);
        table.source = format!("{}+remote", table.source);
    }
    Ok(count)
}

/// Extract Anthropic `claude-*` entries from a LiteLLM-shaped object
/// (`{ "<model>": { "litellm_provider": …, "input_cost_per_token": … } }`).
/// Tolerant by design: unknown keys, non-object entries, other providers,
/// and entries without valid costs are skipped, never fatal. Missing cache
/// cost fields fall back to the published multipliers.
fn parse_litellm(value: &Value) -> HashMap<String, ModelPricing> {
    let Some(entries) = value.as_object() else {
        return HashMap::new();
    };
    let mut models = HashMap::new();
    for (name, entry) in entries {
        if !name.starts_with("claude") {
            continue;
        }
        let Some(entry) = entry.as_object() else {
            continue;
        };
        // Bedrock/Vertex variants of the same models carry region-scoped
        // names that never appear in transcripts; keep first-party only.
        // Entries without a provider field (our own bundled format before
        // filtering, hypothetical future shapes) are accepted.
        if entry
            .get("litellm_provider")
            .and_then(Value::as_str)
            .is_some_and(|p| p != "anthropic")
        {
            continue;
        }
        let cost = |key: &str| entry.get(key).and_then(Value::as_f64).filter(|c| *c >= 0.0);
        let (Some(input), Some(output)) =
            (cost("input_cost_per_token"), cost("output_cost_per_token"))
        else {
            continue;
        };
        let pricing = ModelPricing {
            input_cost_per_token: input,
            output_cost_per_token: output,
            cache_read_cost_per_token: cost("cache_read_input_token_cost")
                .unwrap_or(input * CACHE_READ_MULTIPLIER),
            cache_write_5m_cost_per_token: cost("cache_creation_input_token_cost")
                .unwrap_or(input * CACHE_WRITE_5M_MULTIPLIER),
            cache_write_1h_cost_per_token: cost("cache_creation_input_token_cost_above_1hr")
                .unwrap_or(input * CACHE_WRITE_1H_MULTIPLIER),
        };
        if pricing.is_sane() {
            models.insert(name.clone(), pricing);
        }
    }
    models
}

/// On-disk cache: resolved per-token costs (not LiteLLM-shaped — the
/// fallback multipliers are already applied at fetch time).
#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema: u32,
    fetched_at_ms: i64,
    models: HashMap<String, ModelPricing>,
}

/// `Ok(None)` when the file doesn't exist; `Err` describes why an existing
/// file was rejected.
fn read_cache(path: &Path) -> Result<Option<HashMap<String, ModelPricing>>, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let cache: CacheFile = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if cache.schema != CACHE_SCHEMA {
        return Err(format!("unsupported cache schema {}", cache.schema));
    }
    let models: HashMap<String, ModelPricing> = cache
        .models
        .into_iter()
        .filter(|(_, pricing)| pricing.is_sane())
        .collect();
    if models.is_empty() {
        return Err("cache holds no valid models".into());
    }
    Ok(Some(models))
}

/// Write-then-rename so a crash mid-write never leaves a truncated cache
/// (same pattern as `settings_merge::write_atomic`).
fn write_cache(path: &Path, models: &HashMap<String, ModelPricing>) -> std::io::Result<()> {
    let fetched_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let cache = CacheFile {
        schema: CACHE_SCHEMA,
        fetched_at_ms,
        models: models.clone(),
    };
    let contents = serde_json::to_vec_pretty(&cache).expect("cache file serializes");
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, &contents)?;
    std::fs::rename(&tmp, path)
}

/// `"claude-sonnet-4-5-20250929"` → `Some("claude-sonnet-4-5")`. Requires a
/// trailing `-` + exactly 8 digits.
fn strip_date_suffix(name: &str) -> Option<&str> {
    let (head, tail) = name.rsplit_once('-')?;
    if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) && !head.is_empty() {
        Some(head)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    use axum::routing::get;
    use axum::Router;

    use crate::transcript;

    const EPS: f64 = 1e-12;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < EPS, "expected {b}, got {a}");
    }

    // ---- bundled data ----------------------------------------------------

    #[test]
    fn bundled_covers_current_model_families() {
        let table = PricingTable::bundled();
        // Every model observed in the local transcript corpus, plus legacy
        // claude-3 generation: opus, sonnet, haiku, fable families.
        for model in [
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-fable-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-3-haiku-20240307",
            "claude-3-opus-20240229",
            "claude-3-7-sonnet-20250219",
        ] {
            assert!(table.lookup(model).is_some(), "missing pricing: {model}");
        }
        assert!(
            table.len() >= 20,
            "bundled table too small: {}",
            table.len()
        );
    }

    #[test]
    fn bundled_cache_costs_match_published_multipliers() {
        let table = PricingTable::bundled();
        // claude-fable-5 carries all explicit cache fields in the snapshot;
        // they sit exactly on Anthropic's published multipliers.
        let p = table.lookup("claude-fable-5").unwrap();
        approx(p.input_cost_per_token, 1e-5);
        approx(p.output_cost_per_token, 5e-5);
        approx(p.cache_read_cost_per_token, p.input_cost_per_token * 0.1);
        approx(
            p.cache_write_5m_cost_per_token,
            p.input_cost_per_token * 1.25,
        );
        approx(
            p.cache_write_1h_cost_per_token,
            p.input_cost_per_token * 2.0,
        );
    }

    #[test]
    fn missing_cache_fields_fall_back_to_multipliers() {
        // claude-sonnet-4-6 has no `cache_creation_input_token_cost_above_1hr`
        // in the snapshot → 2.0× input fallback.
        let table = PricingTable::bundled();
        let p = table.lookup("claude-sonnet-4-6").unwrap();
        approx(
            p.cache_write_1h_cost_per_token,
            p.input_cost_per_token * 2.0,
        );

        // Entry with no cache fields at all → every fallback applies.
        let value: Value = serde_json::from_str(
            r#"{"claude-test": {"litellm_provider": "anthropic",
                "input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5}}"#,
        )
        .unwrap();
        let models = parse_litellm(&value);
        let p = &models["claude-test"];
        approx(p.cache_read_cost_per_token, 4e-7);
        approx(p.cache_write_5m_cost_per_token, 5e-6);
        approx(p.cache_write_1h_cost_per_token, 8e-6);
    }

    #[test]
    fn parse_litellm_skips_foreign_and_malformed_entries() {
        let value: Value = serde_json::from_str(
            r#"{
                "sample_spec": {"input_cost_per_token": "see docs"},
                "gpt-99": {"litellm_provider": "openai", "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6},
                "claude-on-bedrock-v1:0": {"litellm_provider": "bedrock", "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6},
                "claude-no-costs": {"litellm_provider": "anthropic"},
                "claude-negative": {"litellm_provider": "anthropic", "input_cost_per_token": -1.0, "output_cost_per_token": 1e-6},
                "claude-good": {"litellm_provider": "anthropic", "input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6},
                "claude-string-costs": {"litellm_provider": "anthropic", "input_cost_per_token": "1e-6", "output_cost_per_token": "5e-6"}
            }"#,
        )
        .unwrap();
        let models = parse_litellm(&value);
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("claude-good"));
    }

    // ---- lookup normalization --------------------------------------------

    #[test]
    fn lookup_normalizes_model_names() {
        let table = PricingTable::bundled();
        let direct = table.lookup("claude-fable-5").unwrap();
        // Provider prefix stripped.
        assert_eq!(table.lookup("anthropic/claude-fable-5"), Some(direct));
        // Dated variant of a dateless key.
        assert_eq!(table.lookup("claude-fable-5-20991231"), Some(direct));
        // Dateless variant of a dated-only key.
        let dated = table.lookup("claude-3-haiku-20240307").unwrap();
        assert_eq!(table.lookup("claude-3-haiku"), Some(dated));
        // Whitespace tolerated; junk rejected.
        assert_eq!(table.lookup(" claude-fable-5 "), Some(direct));
        assert_eq!(table.lookup(""), None);
        assert_eq!(table.lookup("gpt-4o"), None);
    }

    // ---- cost computation -------------------------------------------------

    #[test]
    fn unknown_model_is_flagged_not_priced() {
        let table = PricingTable::bundled();
        let tokens = UsageTokens {
            input_tokens: 100,
            output_tokens: 100,
            ..Default::default()
        };
        assert_eq!(
            table.cost_for(Some("claude-imaginary-99"), &tokens),
            CostOutcome::UnknownModel
        );
        assert_eq!(table.cost_for(None, &tokens), CostOutcome::UnknownModel);
        assert_eq!(table.cost_for(None, &tokens).usd(), None);
        assert_eq!(
            table.cost_for(Some(transcript::SYNTHETIC_MODEL), &tokens),
            CostOutcome::UnknownModel
        );
    }

    #[test]
    fn cache_split_prices_5m_and_1h_separately() {
        let table = PricingTable::bundled();
        // fable-5: write_5m 1.25e-5, write_1h 2e-5.
        let split = UsageTokens {
            cache_creation_tokens: 3_000,
            cache_creation_5m_tokens: Some(1_000),
            cache_creation_1h_tokens: Some(2_000),
            ..Default::default()
        };
        approx(
            table
                .cost_for(Some("claude-fable-5"), &split)
                .usd()
                .unwrap(),
            1_000.0 * 1.25e-5 + 2_000.0 * 2e-5,
        );
        // No split → everything at the default 5m rate.
        let unsplit = UsageTokens {
            cache_creation_tokens: 3_000,
            ..Default::default()
        };
        approx(
            table
                .cost_for(Some("claude-fable-5"), &unsplit)
                .usd()
                .unwrap(),
            3_000.0 * 1.25e-5,
        );
        // Partial split: the unsplit remainder prices at the 5m rate.
        let partial = UsageTokens {
            cache_creation_tokens: 3_000,
            cache_creation_5m_tokens: Some(1_000),
            cache_creation_1h_tokens: None,
            ..Default::default()
        };
        approx(
            table
                .cost_for(Some("claude-fable-5"), &partial)
                .usd()
                .unwrap(),
            3_000.0 * 1.25e-5,
        );
    }

    #[test]
    fn fixture_session_costs_match_hand_calculated_values() {
        // tests/fixtures/transcripts/main-session.jsonl: 2 requests on
        // claude-fable-5 ($10/M input, $50/M output, $1/M cache read,
        // $20/M 1h cache write), all cache writes 1h-TTL.
        let table = PricingTable::bundled();
        let parse =
            transcript::parse_file(Path::new("tests/fixtures/transcripts/main-session.jsonl"))
                .unwrap();
        let requests = transcript::collapse_requests(&parse.lines);
        assert_eq!(requests.len(), 2);

        // req_011Cbwf9sGnBjoiZz25k4EK8:
        //   17045 in            × 0.00001  = 0.17045
        //   94 out              × 0.00005  = 0.0047
        //   23661 cache read    × 0.000001 = 0.023661
        //   31356 cache 1h write× 0.00002  = 0.62712
        //                                  Σ 0.825931
        let r1 = &requests[0];
        assert_eq!(
            r1.request_id.as_deref(),
            Some("req_011Cbwf9sGnBjoiZz25k4EK8")
        );
        let c1 = table
            .cost_for(r1.model.as_deref(), &UsageTokens::from(r1))
            .usd()
            .unwrap();
        approx(c1, 0.825931);

        // req_011CbwfAFuopq3NdmbdDHmd2:
        //   2 in                × 0.00001  = 0.00002
        //   453 out             × 0.00005  = 0.02265
        //   55017 cache read    × 0.000001 = 0.055017
        //   24494 cache 1h write× 0.00002  = 0.48988
        //                                  Σ 0.567567
        let r2 = &requests[1];
        assert_eq!(
            r2.request_id.as_deref(),
            Some("req_011CbwfAFuopq3NdmbdDHmd2")
        );
        let c2 = table
            .cost_for(r2.model.as_deref(), &UsageTokens::from(r2))
            .usd()
            .unwrap();
        approx(c2, 0.567567);
    }

    // ---- cache file -------------------------------------------------------

    fn sample_pricing(input: f64) -> ModelPricing {
        ModelPricing {
            input_cost_per_token: input,
            output_cost_per_token: input * 5.0,
            cache_read_cost_per_token: input * 0.1,
            cache_write_5m_cost_per_token: input * 1.25,
            cache_write_1h_cost_per_token: input * 2.0,
        }
    }

    #[test]
    fn cache_file_overlays_bundled_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut models = HashMap::new();
        // A model the bundle doesn't know…
        models.insert("claude-from-cache-1".to_string(), sample_pricing(7e-6));
        // …and a price override for a bundled model.
        models.insert("claude-fable-5".to_string(), sample_pricing(9e-6));
        write_cache(&cache_path(dir.path()), &models).unwrap();

        let table = PricingTable::load(dir.path());
        assert_eq!(table.source, "bundled+cache");
        assert!(table.lookup("claude-from-cache-1").is_some());
        approx(
            table.lookup("claude-fable-5").unwrap().input_cost_per_token,
            9e-6,
        );
        // Bundled models not in the cache survive the overlay.
        assert!(table.lookup("claude-opus-4-8").is_some());
    }

    #[test]
    fn corrupt_or_missing_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        // Missing file: pure bundled.
        assert_eq!(PricingTable::load(dir.path()), PricingTable::bundled());
        // Corrupt file: pure bundled, no panic.
        std::fs::write(cache_path(dir.path()), b"not json {").unwrap();
        assert_eq!(PricingTable::load(dir.path()), PricingTable::bundled());
        // Wrong schema version: rejected.
        std::fs::write(
            cache_path(dir.path()),
            br#"{"schema": 999, "fetched_at_ms": 0, "models": {}}"#,
        )
        .unwrap();
        assert_eq!(PricingTable::load(dir.path()), PricingTable::bundled());
    }

    // ---- remote refresh ----------------------------------------------------

    async fn serve_body(body: &'static str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/pricing.json", get(move || async move { body }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn refresh_updates_table_and_cache_on_valid_payload() {
        let addr = serve_body(
            r#"{
                "claude-brand-new-1": {"litellm_provider": "anthropic",
                    "input_cost_per_token": 2e-6, "output_cost_per_token": 1e-5,
                    "cache_read_input_token_cost": 2e-7,
                    "cache_creation_input_token_cost": 2.5e-6,
                    "cache_creation_input_token_cost_above_1hr": 4e-6}
            }"#,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let state = PricingState::new(PricingTable::bundled());
        let cache_file = cache_path(dir.path());

        let count = refresh_once(&format!("http://{addr}/pricing.json"), &state, &cache_file)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // In-memory table picked up the new model without losing bundled ones.
        let table = state.0.read().unwrap();
        assert!(table.lookup("claude-brand-new-1").is_some());
        assert!(table.lookup("claude-opus-4-8").is_some());
        assert_eq!(table.source, "bundled+remote");
        drop(table);

        // Cache file written and loadable: a fresh load sees the new model.
        let reloaded = PricingTable::load(dir.path());
        approx(
            reloaded
                .lookup("claude-brand-new-1")
                .unwrap()
                .input_cost_per_token,
            2e-6,
        );
    }

    #[tokio::test]
    async fn refresh_rejects_invalid_payloads_without_touching_state() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = cache_path(dir.path());
        // Pre-existing good cache that a bad refresh must not clobber.
        let mut existing = HashMap::new();
        existing.insert("claude-keep-me".to_string(), sample_pricing(1e-6));
        write_cache(&cache_file, &existing).unwrap();
        let before = std::fs::read(&cache_file).unwrap();

        for body in [
            "not json at all",
            "[1, 2, 3]",
            r#"{"gpt-99": {"litellm_provider": "openai", "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6}}"#,
            "{}",
        ] {
            let addr = serve_body(body).await;
            let state = PricingState::new(PricingTable::bundled());
            let snapshot = state.0.read().unwrap().clone();
            let result =
                refresh_once(&format!("http://{addr}/pricing.json"), &state, &cache_file).await;
            assert!(result.is_err(), "payload should be rejected: {body}");
            assert_eq!(*state.0.read().unwrap(), snapshot, "table mutated: {body}");
            assert_eq!(
                std::fs::read(&cache_file).unwrap(),
                before,
                "cache clobbered: {body}"
            );
        }
    }

    #[tokio::test]
    async fn refresh_is_fail_silent_when_remote_unreachable() {
        // Bind-then-drop guarantees a closed port: connection refused, fast.
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let dir = tempfile::tempdir().unwrap();
        let state = PricingState::new(PricingTable::bundled());
        let snapshot = state.0.read().unwrap().clone();
        let result = refresh_once(
            &format!("http://{addr}/pricing.json"),
            &state,
            &cache_path(dir.path()),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(*state.0.read().unwrap(), snapshot);
        assert!(!cache_path(dir.path()).exists());
        // The spawned wrapper swallows the same error without panicking.
        refresh(state, dir.path().to_path_buf()).await;
    }

    #[test]
    fn pricing_state_cost_for_matches_table() {
        let state = PricingState::new(PricingTable::bundled());
        let tokens = UsageTokens {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        // $10/M input on fable-5.
        approx(
            state
                .cost_for(Some("claude-fable-5"), &tokens)
                .usd()
                .unwrap(),
            10.0,
        );
        assert_eq!(
            state.cost_for(Some("claude-imaginary-99"), &tokens),
            CostOutcome::UnknownModel
        );
    }
}
