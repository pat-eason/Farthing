//! Subscription / Claude Max usage-limits polling and display-mode ownership.
//!
//! This is an opt-in third data source alongside OTel live ingest and transcript
//! backfill. When enabled it polls `GET https://api.anthropic.com/api/oauth/usage`
//! (with a Bearer token read from the macOS keychain) to surface rolling-window
//! utilization percentages for Claude Max / Pro plans. Unlike the other two
//! sources it requires no DB migration — all persistence goes through the `meta`
//! table as JSON blobs:
//!
//! - **`usage_limits_config`** — opt-in flag and display mode preference.
//! - **`usage_limits_snapshot`** — the last normalized snapshot, seeded into
//!   managed state at startup so the UI sees something while the first refresh
//!   is in-flight.
//!
//! Design constraints:
//! - The keychain is read-only; we never write to it.
//! - Network errors and 4xx/5xx responses are fail-silent: the caller's polling
//!   interval handles retries. 429 is additionally back-pressured (we log and
//!   return immediately; the interval already enforces a 5-minute minimum gap).
//! - The polling interval itself lives in `lib.rs`, not here. This module only
//!   provides `refresh` (the single fetch-and-persist operation) and the Tauri
//!   commands the frontend calls.
//! - The token value is never logged.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::db::DbState;

// ---- meta keys & events ----

const CONFIG_KEY: &str = "usage_limits_config";
const SNAPSHOT_KEY: &str = "usage_limits_snapshot";

/// Emitted after every successful or terminal-failure `refresh` call with the
/// current [`UsageSnapshot`] payload. The frontend listens for this to update
/// its subscription-usage display.
pub const USAGE_UPDATED_EVENT: &str = "usage:updated";

/// Emitted by `display_mode_set` and `usage_limits_config_set` (when disabling)
/// so every view that shows different content based on display mode can switch
/// immediately. Payload is the new mode string: `"api"` or `"subscription"`.
pub const DISPLAY_MODE_CHANGED_EVENT: &str = "display:mode-changed";

/// Whole-request timeout for the usage API call.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ---- raw API response types (version-tolerant, no deny_unknown_fields) ----

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct UsageBucket {
    utilization: Option<f64>,
    resets_at: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ExtraUsageRaw {
    is_enabled: Option<bool>,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct UsageApiResponse {
    five_hour: Option<UsageBucket>,
    seven_day: Option<UsageBucket>,
    seven_day_sonnet: Option<UsageBucket>,
    seven_day_opus: Option<UsageBucket>,
    extra_usage: Option<ExtraUsageRaw>,
}

// ---- normalized snapshot types (UI-facing, persisted) ----

/// Whether the last refresh attempt succeeded, hit an auth error, or was
/// unable to reach the service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    Unauthenticated,
    Unavailable,
}

/// Normalized state for one rolling-window bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSnapshot {
    /// Human-readable label for the window, e.g. `"5h session"`.
    pub label: String,
    /// Utilization percentage (0–100), `None` if the API did not return one.
    pub percent: Option<f64>,
    /// When the window resets, as milliseconds since the Unix epoch, or `None`
    /// if not available / not parseable.
    pub resets_at_ms: Option<i64>,
}

/// Normalized extra-usage (overage / add-on credits) snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraUsageSnapshot {
    pub is_enabled: bool,
    pub monthly_limit: Option<f64>,
    pub used_credits: Option<f64>,
    pub utilization: Option<f64>,
}

/// The full normalized usage snapshot persisted to `meta` and emitted on every
/// refresh cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub five_hour: WindowSnapshot,
    pub seven_day: WindowSnapshot,
    pub seven_day_sonnet: WindowSnapshot,
    pub seven_day_opus: WindowSnapshot,
    pub extra_usage: Option<ExtraUsageSnapshot>,
    /// Wall-clock UTC milliseconds when this snapshot was fetched.
    pub fetched_at_ms: i64,
    pub status: UsageStatus,
}

// ---- config types ----

fn default_display_mode() -> DisplayMode {
    DisplayMode::Api
}

/// Whether the frontend should display API-style cost (default) or
/// subscription utilization percentages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    Api,
    Subscription,
}

/// Persisted config for the usage-limits feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLimitsConfig {
    /// Whether background polling is enabled. Defaults to `false`; the user
    /// must opt in.
    pub enabled: bool,
    /// Current display mode preference.
    pub display_mode: DisplayMode,
}

impl Default for UsageLimitsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            display_mode: default_display_mode(),
        }
    }
}

// ---- managed state ----

/// Shared in-memory snapshot, updated by `refresh` and seeded from `meta` at
/// startup. `None` until the first refresh completes or `seed_from_meta`
/// supplies a previous snapshot.
pub type UsageLimitsState = Arc<std::sync::RwLock<Option<UsageSnapshot>>>;

// ---- keychain helper ----

/// Read the Claude Code OAuth access token from the macOS keychain.
///
/// Shells out to `security find-generic-password -s "Claude Code-credentials" -w`,
/// which returns a JSON string. Parses the JSON and extracts
/// `claudeAiOauth.accessToken`. Returns `Err(String)` on any failure.
/// The token value is never logged.
fn read_keychain_token() -> Result<String, String> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .map_err(|e| format!("keychain: security command failed to launch: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("keychain: security exited with error: {stderr}"));
    }

    let raw = String::from_utf8(output.stdout)
        .map_err(|e| format!("keychain: output is not UTF-8: {e}"))?;
    let raw = raw.trim();

    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| format!("keychain: JSON parse failed: {e}"))?;

    value
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "keychain: missing claudeAiOauth.accessToken".into())
}

// ---- resets_at normalization ----

/// Convert the `resets_at` field (either an ISO-8601 string or an
/// epoch-seconds number) to milliseconds since the Unix epoch.
/// Returns `None` if the value is absent, the wrong type, or unparseable.
fn normalize_resets_at(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => {
            // Epoch seconds → epoch milliseconds.
            let secs = n.as_f64()?;
            Some((secs * 1000.0) as i64)
        }
        serde_json::Value::String(s) => {
            // Try chrono first (it's in Cargo.toml with `clock` feature).
            // The expected format is "2026-06-13T04:10:00+00:00" (RFC 3339).
            use chrono::DateTime;
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis());
            }
            // Fallback: try parsing as a bare ISO date "YYYY-MM-DDTHH:MM:SS"
            // without timezone (treat as UTC).
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
                return Some(dt.and_utc().timestamp_millis());
            }
            None
        }
        _ => None,
    }
}

// ---- current time helper ----

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---- window normalization ----

fn normalize_window(bucket: Option<&UsageBucket>, label: &str) -> WindowSnapshot {
    let (percent, resets_at_ms) = match bucket {
        Some(b) => {
            let resets_at_ms = b
                .resets_at
                .as_ref()
                .and_then(normalize_resets_at);
            (b.utilization, resets_at_ms)
        }
        None => (None, None),
    };
    WindowSnapshot {
        label: label.to_string(),
        percent,
        resets_at_ms,
    }
}

// ---- meta persistence helpers ----

/// Read `UsageLimitsConfig` from `meta`. Returns the default config on any
/// read or parse error.
pub fn read_config(db: &DbState) -> UsageLimitsConfig {
    let conn = db.0.lock().expect("db mutex poisoned");
    conn.conn()
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [CONFIG_KEY],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// Persist `config` to the `meta` table.
pub fn write_config(db: &DbState, config: &UsageLimitsConfig) -> Result<(), String> {
    let json =
        serde_json::to_string(config).map_err(|e| format!("cannot serialize config: {e}"))?;
    let conn = db.0.lock().expect("db mutex poisoned");
    conn.conn()
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            rusqlite::params![CONFIG_KEY, json],
        )
        .map_err(|e| format!("cannot write usage_limits_config: {e}"))?;
    Ok(())
}

/// Read the last persisted snapshot from `meta`. Returns `None` on any error.
fn read_snapshot(db: &DbState) -> Option<UsageSnapshot> {
    let conn = db.0.lock().expect("db mutex poisoned");
    let raw: String = conn
        .conn()
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [SNAPSHOT_KEY],
            |row| row.get(0),
        )
        .ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist `snapshot` to the `meta` table.
fn write_snapshot(db: &DbState, snapshot: &UsageSnapshot) -> Result<(), String> {
    let json =
        serde_json::to_string(snapshot).map_err(|e| format!("cannot serialize snapshot: {e}"))?;
    let conn = db.0.lock().expect("db mutex poisoned");
    conn.conn()
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            rusqlite::params![SNAPSHOT_KEY, json],
        )
        .map_err(|e| format!("cannot write usage_limits_snapshot: {e}"))?;
    Ok(())
}

// ---- startup seed ----

/// Synchronous startup helper: seed managed state with the last persisted
/// snapshot so the frontend sees data immediately. Returns `None` when there
/// is no persisted snapshot or it fails to deserialize.
pub fn seed_from_meta(db: &DbState) -> Option<UsageSnapshot> {
    read_snapshot(db)
}

/// Returns `true` if the user has ever explicitly saved a `UsageLimitsConfig`
/// (even a default one). Used by onboarding to decide whether to show the
/// mode-choice step.
pub fn is_mode_chosen(db: &DbState) -> bool {
    let conn = db.0.lock().expect("db mutex poisoned");
    conn.conn()
        .query_row(
            "SELECT 1 FROM meta WHERE key = ?1",
            [CONFIG_KEY],
            |_| Ok(()),
        )
        .is_ok()
}

// ---- core refresh ----

/// Perform one fetch-and-persist cycle against the Anthropic usage API.
///
/// Guards against concurrent calls via `in_flight`: if another call is
/// already running the function returns immediately. The caller (the polling
/// loop in `lib.rs`) enforces a minimum 5-minute interval between calls;
/// this function does not re-check that interval.
///
/// After a successful or terminal-failure fetch the resulting [`UsageSnapshot`]
/// is written to managed state, persisted to `meta`, and emitted as a
/// `usage:updated` event. On 429 the function logs a warning and returns
/// without updating state so the interval alone controls the retry cadence.
pub async fn refresh<R: tauri::Runtime>(
    in_flight: Arc<AtomicBool>,
    state: &UsageLimitsState,
    db: &DbState,
    app: &tauri::AppHandle<R>,
) {
    // Single-flight guard: atomically claim the slot, clear it on exit
    // (including on panic) via a drop guard.
    if in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    struct InFlightGuard(Arc<AtomicBool>);
    impl Drop for InFlightGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _g = InFlightGuard(Arc::clone(&in_flight));

    // Check config.
    let config = read_config(db);
    if !config.enabled {
        return;
    }

    // Read token from keychain.
    let token = match read_keychain_token() {
        Ok(t) => t,
        Err(err) => {
            eprintln!("usage_limits: keychain read failed: {err}");
            let snapshot = terminal_snapshot(UsageStatus::Unauthenticated);
            store_and_emit(state, db, app, snapshot);
            return;
        }
    };

    // Build reqwest client and fetch.
    let client = match reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("usage_limits: cannot build HTTP client: {e}");
            let snapshot = terminal_snapshot(UsageStatus::Unavailable);
            store_and_emit(state, db, app, snapshot);
            return;
        }
    };

    let response = match client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("usage_limits: network error: {e}");
            let snapshot = terminal_snapshot(UsageStatus::Unavailable);
            store_and_emit(state, db, app, snapshot);
            return;
        }
    };

    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        let snapshot = terminal_snapshot(UsageStatus::Unauthenticated);
        store_and_emit(state, db, app, snapshot);
        return;
    }

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        eprintln!("usage_limits: rate-limited (429); skipping until next interval");
        return;
    }

    if !status.is_success() {
        eprintln!("usage_limits: HTTP {status}; treating as unavailable");
        let snapshot = terminal_snapshot(UsageStatus::Unavailable);
        store_and_emit(state, db, app, snapshot);
        return;
    }

    // Parse response body.
    let body = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("usage_limits: failed to read response body: {e}");
            let snapshot = terminal_snapshot(UsageStatus::Unavailable);
            store_and_emit(state, db, app, snapshot);
            return;
        }
    };

    let api_resp: UsageApiResponse = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("usage_limits: failed to parse response JSON: {e}");
            let snapshot = terminal_snapshot(UsageStatus::Unavailable);
            store_and_emit(state, db, app, snapshot);
            return;
        }
    };

    let snapshot = normalize_response(api_resp);
    store_and_emit(state, db, app, snapshot);
}

/// Build a terminal (error) snapshot with no window data.
fn terminal_snapshot(status: UsageStatus) -> UsageSnapshot {
    let empty = |label: &str| WindowSnapshot {
        label: label.to_string(),
        percent: None,
        resets_at_ms: None,
    };
    UsageSnapshot {
        five_hour: empty("5h session"),
        seven_day: empty("7d overall"),
        seven_day_sonnet: empty("7d Sonnet"),
        seven_day_opus: empty("7d Opus"),
        extra_usage: None,
        fetched_at_ms: now_ms(),
        status,
    }
}

/// Normalize a successful `UsageApiResponse` into a `UsageSnapshot`.
fn normalize_response(resp: UsageApiResponse) -> UsageSnapshot {
    let five_hour = normalize_window(resp.five_hour.as_ref(), "5h session");
    let seven_day = normalize_window(resp.seven_day.as_ref(), "7d overall");
    let seven_day_sonnet = normalize_window(resp.seven_day_sonnet.as_ref(), "7d Sonnet");
    let seven_day_opus = normalize_window(resp.seven_day_opus.as_ref(), "7d Opus");

    let extra_usage = resp.extra_usage.map(|e| ExtraUsageSnapshot {
        is_enabled: e.is_enabled.unwrap_or(false),
        monthly_limit: e.monthly_limit,
        used_credits: e.used_credits,
        utilization: e.utilization,
    });

    UsageSnapshot {
        five_hour,
        seven_day,
        seven_day_sonnet,
        seven_day_opus,
        extra_usage,
        fetched_at_ms: now_ms(),
        status: UsageStatus::Ok,
    }
}

/// Store `snapshot` in managed state, persist to `meta`, and emit the event.
fn store_and_emit<R: tauri::Runtime>(
    state: &UsageLimitsState,
    db: &DbState,
    app: &tauri::AppHandle<R>,
    snapshot: UsageSnapshot,
) {
    if let Ok(mut guard) = state.write() {
        *guard = Some(snapshot.clone());
    }
    if let Err(e) = write_snapshot(db, &snapshot) {
        eprintln!("usage_limits: cannot persist snapshot: {e}");
    }
    let _ = app.emit(USAGE_UPDATED_EVENT, &snapshot);
}

// ---- Tauri commands ----

/// Return the current in-memory snapshot, or `None` if no refresh has
/// completed yet.
#[tauri::command]
pub async fn usage_limits_status(
    state: tauri::State<'_, UsageLimitsState>,
) -> Result<Option<UsageSnapshot>, String> {
    Ok(state
        .read()
        .map_err(|_| "usage_limits state lock poisoned".to_string())?
        .clone())
}

/// Return the current usage-limits config from the database.
#[tauri::command]
pub async fn usage_limits_config_get(
    db: tauri::State<'_, DbState>,
) -> Result<UsageLimitsConfig, String> {
    Ok(read_config(&db))
}

/// Persist a new usage-limits config.
///
/// When enabling: spawns an immediate background refresh.
/// When disabling with `display_mode == Subscription`: also resets
/// `display_mode` to `Api` and emits `display:mode-changed` with `"api"`.
#[tauri::command]
pub async fn usage_limits_config_set(
    mut config: UsageLimitsConfig,
    db: tauri::State<'_, DbState>,
    state: tauri::State<'_, UsageLimitsState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Coupling rule: Subscription display mode requires the poller to be
    // enabled. Force it back to Api when disabling.
    if !config.enabled && config.display_mode == DisplayMode::Subscription {
        config.display_mode = DisplayMode::Api;
    }

    // When disabling, the display mode has been forced to Api above; emit the
    // mode-changed event so views switch immediately.
    let was_disabled = !config.enabled;

    write_config(&db, &config)?;

    if was_disabled {
        let _ = app.emit(DISPLAY_MODE_CHANGED_EVENT, "api");
    }

    if config.enabled {
        // Immediate refresh in the background; borrow the Arc from DbState.
        let state_arc = state.inner().clone();
        let db_arc = Arc::clone(&db.0);
        let in_flight = Arc::new(AtomicBool::new(false));
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            let db_wrapped = DbState(db_arc);
            refresh(in_flight, &state_arc, &db_wrapped, &app_clone).await;
        });
    }

    Ok(())
}

/// Return the current display mode from the database.
#[tauri::command]
pub async fn display_mode_get(db: tauri::State<'_, DbState>) -> Result<DisplayMode, String> {
    Ok(read_config(&db).display_mode)
}

/// Set the display mode.
///
/// If switching to `Subscription` and the poller is not yet enabled, enables
/// it and triggers an immediate refresh. Always emits `display:mode-changed`
/// with the new mode string.
#[tauri::command]
pub async fn display_mode_set(
    mode: DisplayMode,
    db: tauri::State<'_, DbState>,
    state: tauri::State<'_, UsageLimitsState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut config = read_config(&db);
    let previously_enabled = config.enabled;
    config.display_mode = mode.clone();

    // Switching to Subscription requires the poller; enable it implicitly.
    if mode == DisplayMode::Subscription && !config.enabled {
        config.enabled = true;
    }

    write_config(&db, &config)?;

    let mode_str = match &mode {
        DisplayMode::Api => "api",
        DisplayMode::Subscription => "subscription",
    };
    let _ = app.emit(DISPLAY_MODE_CHANGED_EVENT, mode_str);

    // If we just enabled the poller (it wasn't running before), kick off a
    // refresh immediately so the UI doesn't have to wait for the next tick.
    if config.enabled && !previously_enabled {
        let state_arc = state.inner().clone();
        let db_arc = Arc::clone(&db.0);
        let in_flight = Arc::new(AtomicBool::new(false));
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            let db_wrapped = DbState(db_arc);
            refresh(in_flight, &state_arc, &db_wrapped, &app_clone).await;
        });
    }

    Ok(())
}
