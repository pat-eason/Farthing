//! Alert configuration + runtime state (Unit 3 of the cost-notifications plan).
//!
//! [`AlertState`] is the managed home of the cost-alert feature, modelled on
//! [`crate::capture::CaptureState`]: a database handle plus an in-memory cache,
//! persisted to the `meta` table so settings and dedup bookkeeping survive
//! restarts. Where `CaptureState` persists a single `AtomicBool`, the alert
//! feature carries a composite blob, so two JSON values are stored:
//!
//! - **config** under [`ALERT_CONFIG_KEY`]: what the user tuned in the Spend UI
//!   (the delta + burst rules, their quiet windows, the API-billing copy flag).
//!   The shape is left open for the Budgets plan to add budget-derived
//!   approach/breach config without a migration (it is JSON in `meta`).
//! - **runtime** under [`ALERT_RUNTIME_KEY`]: edge-trigger / cooldown bookkeeping
//!   the engine reads and writes each evaluation (last delta step fired, burst
//!   cooldown deadline, the permission-lost signal).
//!
//! Two pieces of state are deliberately *not* persisted:
//!
//! - **`process_start_ms`**: the wall-clock instant this process launched. It
//!   floors the burst/delta spend queries so spend recovered from before launch
//!   (backfill, or an otel re-delivery that resets a row's `timestamp_ms`) can
//!   never trip a live alert. It is meaningful only for the current process, so
//!   it lives in memory and is re-captured at every startup.
//! - the **eval lock** (`Mutex<()>`): held across the whole gather→evaluate→
//!   persist cycle so concurrent ingest-path, 60s-tick, and config-save
//!   evaluations are mutually exclusive. The DB mutex serializes individual
//!   *statements*, not the read-modify-write of the cached runtime blob; without
//!   this lock two evaluations could interleave into a lost update (a double-fire
//!   or a dropped flag). See the plan's concurrency decision.
//!
//! Loading is resilient (mirroring [`CaptureState::load`]): an absent or
//! malformed JSON row falls back to documented defaults rather than failing
//! startup or panicking on garbage.

use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, Runtime};

use crate::db::Db;

/// `meta` key holding the persisted alert config JSON ([`AlertConfig`]).
pub const ALERT_CONFIG_KEY: &str = "alert_config";

/// `meta` key holding the persisted alert runtime JSON ([`AlertRuntime`]).
pub const ALERT_RUNTIME_KEY: &str = "alert_runtime";

/// Event emitted to the frontend whenever the alert config changes (Spend UI
/// save); payload is the resulting [`AlertConfig`].
pub const ALERT_CONFIG_CHANGED_EVENT: &str = "alert:config-changed";

// ---- defaults (per the plan; tunable by the user once shipped) ----

fn default_delta_step_usd() -> f64 {
    50.0
}
fn default_burst_threshold_usd() -> f64 {
    10.0
}
fn default_burst_window_minutes() -> u32 {
    10
}
fn default_burst_cooldown_minutes() -> u32 {
    15
}
/// Burst is the only rule enabled by default: a runaway agent loop is the
/// motivating day-one danger, and a 10-minute window keeps it from firing on a
/// legitimately heavy session. Delta is opt-in (a milestone signal, not a guard).
fn default_burst_enabled() -> bool {
    true
}

/// A quiet-hours window, as wall-clock local times the engine compares against
/// `Local::now()`. Stored as `"HH:MM"` 24-hour strings so the Spend UI can bind
/// a native `<input type="time">` to each end with no conversion. A wrap-around
/// window (`start` later than `end`, e.g. 22:00–07:00) means "overnight" and is
/// resolved wrap-aware by the engine (Unit 4); `start == end` is treated as
/// unset by the UI and never stored as a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietWindow {
    /// Inclusive start of quiet hours, local `"HH:MM"`.
    pub start: String,
    /// Exclusive end of quiet hours, local `"HH:MM"`.
    pub end: String,
}

/// The recurring-delta rule ("every $N of spend"). Disabled by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Spend increment between milestones, in API-equivalent USD.
    #[serde(default = "default_delta_step_usd")]
    pub step_usd: f64,
    /// Optional per-rule quiet hours; `None` means always allowed.
    #[serde(default)]
    pub quiet: Option<QuietWindow>,
}

impl Default for DeltaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            step_usd: default_delta_step_usd(),
            quiet: None,
        }
    }
}

/// The session/burst rate rule ("$N in a rolling window"). Enabled by default to
/// guard against a runaway loop on day one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurstConfig {
    #[serde(default = "default_burst_enabled")]
    pub enabled: bool,
    /// Spend within the rolling window that arms the alert, in API-equivalent USD.
    #[serde(default = "default_burst_threshold_usd")]
    pub threshold_usd: f64,
    /// Width of the rolling window, in minutes.
    #[serde(default = "default_burst_window_minutes")]
    pub window_minutes: u32,
    /// Minimum gap between burst fires, in minutes (one alert per runaway loop).
    #[serde(default = "default_burst_cooldown_minutes")]
    pub cooldown_minutes: u32,
    /// Optional per-rule quiet hours; `None` means always allowed.
    #[serde(default)]
    pub quiet: Option<QuietWindow>,
}

impl Default for BurstConfig {
    fn default() -> Self {
        Self {
            enabled: default_burst_enabled(),
            threshold_usd: default_burst_threshold_usd(),
            window_minutes: default_burst_window_minutes(),
            cooldown_minutes: default_burst_cooldown_minutes(),
            quiet: None,
        }
    }
}

/// The full alert configuration persisted under [`ALERT_CONFIG_KEY`]. Every
/// field carries a serde default so partial or older JSON deserializes into the
/// documented defaults instead of erroring; the Budgets plan can add fields here
/// without a migration. `#[serde(default)]` on the struct itself means a missing
/// nested object falls back to that type's `Default`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AlertConfig {
    pub delta: DeltaConfig,
    pub burst: BurstConfig,
    /// "I pay per-token" flag: switches alert copy from neutral usage framing to
    /// real-money wording. Off by default (cost is notional for subscribers).
    pub api_billing: bool,
}

/// Delta dedup bookkeeping: which calendar month the baseline tracks and the
/// highest $N step already fired in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeltaRuntime {
    /// Calendar month the `last_step` baseline belongs to (e.g. `"2026-06"`); a
    /// month rollover re-baselines (Unit 4) so steps never carry across months.
    pub month_key: String,
    /// Highest milestone index already fired this month (`floor(MTD / step_usd)`
    /// at the last fire). Backfill re-baselines this silently (Unit 5).
    pub last_step: i64,
}

/// Burst dedup bookkeeping: the cooldown deadline a fire arms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BurstRuntime {
    /// Unix ms before which burst will not fire again (UTC ms so a DST fall-back
    /// repeated local hour can't reopen the cooldown early). `0` means unarmed.
    pub cooldown_until_ms: i64,
}

/// The full alert runtime persisted under [`ALERT_RUNTIME_KEY`]. Like the config
/// it carries serde defaults end-to-end so a partial/garbage row reloads as the
/// zero state rather than panicking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AlertRuntime {
    pub delta: DeltaRuntime,
    pub burst: BurstRuntime,
    /// Set when a `show` returned `PermissionDenied` (the user revoked or never
    /// granted notification permission); surfaced in the Spend UI so silent
    /// non-coverage becomes visible. The orchestrator (Unit 5) writes it.
    pub permission_lost: bool,
}

/// Shared alert state managed in the Tauri app: the database handle for
/// persistence, the in-memory config + runtime caches, the wall-clock launch
/// instant, and the evaluation lock. Cloning shares all of it (the inner state
/// is `Arc`-wrapped), matching [`CaptureState`].
#[derive(Clone)]
pub struct AlertState {
    db: Arc<Mutex<Db>>,
    config: Arc<Mutex<AlertConfig>>,
    runtime: Arc<Mutex<AlertRuntime>>,
    /// Wall-clock unix ms captured at startup; floors the burst/delta queries so
    /// pre-launch spend can never fire a live alert. Process-lifetime only.
    process_start_ms: i64,
    /// Held across the whole evaluate cycle (Unit 5) so evaluations are mutually
    /// exclusive. `()` because it guards a *critical section*, not a value: the
    /// cached config/runtime stay independently lockable for cheap reads.
    eval_lock: Arc<Mutex<()>>,
}

impl AlertState {
    /// Read the persisted config + runtime (defaults on absent/malformed JSON)
    /// and capture `process_start_ms` from the current wall clock. A bad `meta`
    /// row never fails startup, mirroring [`CaptureState::load`].
    pub fn load(db: Arc<Mutex<Db>>) -> Self {
        let config = read_json(&db, ALERT_CONFIG_KEY);
        let runtime = read_json(&db, ALERT_RUNTIME_KEY);
        Self {
            db,
            config: Arc::new(Mutex::new(config)),
            runtime: Arc::new(Mutex::new(runtime)),
            process_start_ms: chrono::Local::now().timestamp_millis(),
            eval_lock: Arc::new(Mutex::new(())),
        }
    }

    /// The wall-clock instant this process launched (unix ms). The burst/delta
    /// queries floor on this so recovered pre-launch spend is excluded.
    pub fn process_start_ms(&self) -> i64 {
        self.process_start_ms
    }

    /// Acquire the evaluation lock for the duration of a gather→evaluate→persist
    /// cycle. Hold the returned guard across the whole cycle; dropping it releases
    /// the critical section. Unit 5's `gather_and_apply` is the only caller.
    pub fn eval_guard(&self) -> MutexGuard<'_, ()> {
        self.eval_lock.lock().expect("alert eval lock poisoned")
    }

    /// Current config (cheap clone of the cached value; no DB read).
    pub fn config(&self) -> AlertConfig {
        self.config.lock().expect("alert config mutex poisoned").clone()
    }

    /// Current runtime (cheap clone of the cached value; no DB read).
    pub fn runtime(&self) -> AlertRuntime {
        self.runtime.lock().expect("alert runtime mutex poisoned").clone()
    }

    /// Persist `config` then update the cache. Write-first (like
    /// [`CaptureState::set_paused`]): if the DB write fails the in-memory cache is
    /// left unchanged so disk and memory never disagree.
    pub fn set_config(&self, config: AlertConfig) -> Result<(), rusqlite::Error> {
        write_json(&self.db, ALERT_CONFIG_KEY, &config)?;
        *self.config.lock().expect("alert config mutex poisoned") = config;
        Ok(())
    }

    /// Persist `runtime` then update the cache. Write-first, same rationale as
    /// [`set_config`](Self::set_config); the engine calls this at the end of each
    /// evaluation under the eval lock.
    pub fn set_runtime(&self, runtime: AlertRuntime) -> Result<(), rusqlite::Error> {
        write_json(&self.db, ALERT_RUNTIME_KEY, &runtime)?;
        *self.runtime.lock().expect("alert runtime mutex poisoned") = runtime;
        Ok(())
    }
}

/// Read `key` from `meta` and deserialize as `T`. An absent row, an unreadable
/// row, or malformed JSON all collapse to `T::default()` — no panic, no startup
/// failure (the resilient-load contract from [`CaptureState::load`]).
fn read_json<T: Default + serde::de::DeserializeOwned>(db: &Mutex<Db>, key: &str) -> T {
    let db = db.lock().expect("db mutex poisoned");
    db.conn()
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

/// Serialize `value` and upsert it into `meta` under `key` (same
/// INSERT…ON CONFLICT DO UPDATE shape `CaptureState` uses). Serialization of a
/// plain config/runtime struct cannot fail, but the result is threaded through so
/// the write is the single fallible step the caller surfaces.
fn write_json<T: Serialize>(db: &Mutex<Db>, key: &str, value: &T) -> Result<(), rusqlite::Error> {
    let json = serde_json::to_string(value).expect("alert state serializes to JSON");
    let db = db.lock().expect("db mutex poisoned");
    db.conn().execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, json],
    )?;
    Ok(())
}

/// Frontend query: the current alert config (Spend UI reads this on mount).
#[tauri::command]
pub fn alert_config_get(state: tauri::State<'_, AlertState>) -> AlertConfig {
    state.config()
}

/// Frontend action: persist a new alert config, re-evaluate, and notify the UI.
///
/// Persists the config, then runs a re-evaluation so a tightened threshold or a
/// just-enabled rule can fire immediately (and a re-baseline takes effect), and
/// emits [`ALERT_CONFIG_CHANGED_EVENT`] so other windows refresh. The
/// re-evaluation goes through [`crate::alerts::reevaluate_after_config_change`]
/// (see its docs) — a seam Unit 5 fills with the real engine; for this unit it
/// is a no-op so config save is fully functional ahead of the engine landing.
#[tauri::command]
pub fn alert_config_set<R: Runtime>(
    app: tauri::AppHandle<R>,
    config: AlertConfig,
) -> Result<AlertConfig, String> {
    let state = app.state::<AlertState>();
    state
        .set_config(config)
        .map_err(|err| format!("cannot persist alert config: {err}"))?;
    let saved = state.config();
    // Re-evaluate under the engine seam so a config change can fire right away
    // (Unit 5 wires the real gather_and_apply; today this is a documented no-op).
    reevaluate_after_config_change(&app);
    let _ = app.emit(ALERT_CONFIG_CHANGED_EVENT, &saved);
    Ok(saved)
}

/// Re-evaluation seam invoked by [`alert_config_set`] after a config save.
///
/// **Unit 5 fills this in.** It will acquire the [`AlertState`] eval lock and run
/// the full `gather_and_apply` cycle (query sums → `evaluate` → `show` → persist
/// runtime) so a config change re-evaluates immediately and atomically with the
/// ingest-path and tick evaluations. For Unit 3 it is intentionally a no-op: the
/// config is already persisted and the change event emitted, so the Spend UI is
/// fully functional before the engine exists. Keeping the call site here means
/// Unit 5 only changes this body, not `alert_config_set`.
fn reevaluate_after_config_change<R: Runtime>(_app: &tauri::AppHandle<R>) {
    // No-op until Unit 5 wires the evaluation engine.
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Listener;

    fn test_db(dir: &tempfile::TempDir) -> Arc<Mutex<Db>> {
        Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()))
    }

    /// Read a raw `meta` value (to assert the on-disk JSON, not just the cache).
    fn read_meta(db: &Mutex<Db>, key: &str) -> Option<String> {
        let db = db.lock().unwrap();
        db.conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .ok()
    }

    #[test]
    fn fresh_database_loads_documented_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));

        let config = state.config();
        // Burst: enabled $10 / 10 min / 15 min cooldown.
        assert!(config.burst.enabled, "burst is enabled by default");
        assert_eq!(config.burst.threshold_usd, 10.0);
        assert_eq!(config.burst.window_minutes, 10);
        assert_eq!(config.burst.cooldown_minutes, 15);
        assert!(config.burst.quiet.is_none());
        // Delta: disabled $50.
        assert!(!config.delta.enabled, "delta is disabled by default");
        assert_eq!(config.delta.step_usd, 50.0);
        assert!(config.delta.quiet.is_none());
        // API-billing copy flag off by default.
        assert!(!config.api_billing);

        // Runtime starts empty / zeroed, permission not lost.
        assert_eq!(state.runtime(), AlertRuntime::default());
        assert!(!state.runtime().permission_lost);
    }

    #[test]
    fn process_start_ms_is_captured_around_load() {
        let before = chrono::Local::now().timestamp_millis();
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));
        let after = chrono::Local::now().timestamp_millis();
        assert!(
            before <= state.process_start_ms() && state.process_start_ms() <= after,
            "process_start_ms must be the load-time wall clock"
        );
    }

    #[test]
    fn set_config_persists_and_round_trips_on_reload() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let state = AlertState::load(Arc::clone(&db));

        let config = AlertConfig {
            delta: DeltaConfig {
                enabled: true,
                step_usd: 25.0,
                quiet: Some(QuietWindow {
                    start: "22:00".into(),
                    end: "07:00".into(),
                }),
            },
            burst: BurstConfig {
                enabled: false,
                threshold_usd: 5.0,
                window_minutes: 5,
                cooldown_minutes: 30,
                quiet: None,
            },
            api_billing: true,
        };
        state.set_config(config.clone()).unwrap();
        // Cache updated immediately.
        assert_eq!(state.config(), config);

        // A fresh load from the same file sees the persisted config.
        let reloaded = AlertState::load(Arc::clone(&db));
        assert_eq!(reloaded.config(), config);
    }

    #[test]
    fn set_runtime_persists_and_round_trips_on_reload() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let state = AlertState::load(Arc::clone(&db));

        let runtime = AlertRuntime {
            delta: DeltaRuntime {
                month_key: "2026-06".into(),
                last_step: 3,
            },
            burst: BurstRuntime {
                cooldown_until_ms: 1_781_150_400_000,
            },
            permission_lost: true,
        };
        state.set_runtime(runtime.clone()).unwrap();
        assert_eq!(state.runtime(), runtime);

        let reloaded = AlertState::load(db);
        assert_eq!(reloaded.runtime(), runtime);
    }

    #[test]
    fn malformed_config_json_falls_back_to_defaults_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        // Garbage in the config row; valid-but-partial in the runtime row.
        write_json(&db, ALERT_RUNTIME_KEY, &"not even an object").unwrap();
        {
            let conn = db.lock().unwrap();
            conn.conn()
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    rusqlite::params![ALERT_CONFIG_KEY, "{ this is not json"],
                )
                .unwrap();
        }

        let state = AlertState::load(db);
        assert_eq!(state.config(), AlertConfig::default());
        assert_eq!(state.runtime(), AlertRuntime::default());
    }

    #[test]
    fn partial_config_json_fills_missing_fields_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        // Only the burst threshold is specified; everything else must default.
        {
            let conn = db.lock().unwrap();
            conn.conn()
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    rusqlite::params![ALERT_CONFIG_KEY, r#"{"burst":{"threshold_usd":7.5}}"#],
                )
                .unwrap();
        }

        let config = AlertState::load(db).config();
        assert_eq!(config.burst.threshold_usd, 7.5, "explicit field honored");
        assert_eq!(config.burst.window_minutes, 10, "missing field defaults");
        assert!(config.burst.enabled, "missing field defaults to true");
        assert_eq!(config.delta.step_usd, 50.0, "missing object defaults");
        assert!(!config.delta.enabled);
    }

    #[test]
    fn config_serializes_to_expected_json() {
        let config = AlertConfig {
            delta: DeltaConfig {
                enabled: true,
                step_usd: 50.0,
                quiet: None,
            },
            burst: BurstConfig {
                enabled: true,
                threshold_usd: 10.0,
                window_minutes: 10,
                cooldown_minutes: 15,
                quiet: Some(QuietWindow {
                    start: "22:00".into(),
                    end: "07:00".into(),
                }),
            },
            api_billing: false,
        };
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::json!({
                "delta": { "enabled": true, "step_usd": 50.0, "quiet": null },
                "burst": {
                    "enabled": true,
                    "threshold_usd": 10.0,
                    "window_minutes": 10,
                    "cooldown_minutes": 15,
                    "quiet": { "start": "22:00", "end": "07:00" }
                },
                "api_billing": false
            })
        );
    }

    #[test]
    fn runtime_serializes_to_expected_json() {
        let runtime = AlertRuntime {
            delta: DeltaRuntime {
                month_key: "2026-06".into(),
                last_step: 2,
            },
            burst: BurstRuntime {
                cooldown_until_ms: 1_781_150_400_000,
            },
            permission_lost: false,
        };
        assert_eq!(
            serde_json::to_value(&runtime).unwrap(),
            serde_json::json!({
                "delta": { "month_key": "2026-06", "last_step": 2 },
                "burst": { "cooldown_until_ms": 1_781_150_400_000_i64 },
                "permission_lost": false
            })
        );
    }

    #[test]
    fn eval_guard_serializes_evaluations() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));
        // Holding the guard means a second non-blocking acquire fails: the lock
        // is genuinely exclusive (the property Unit 5 relies on for atomicity).
        let _held = state.eval_guard();
        assert!(
            state.eval_lock.try_lock().is_err(),
            "eval lock must be exclusive while a guard is held"
        );
    }

    #[test]
    fn alert_config_set_persists_and_emits_via_mock_app() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(AlertState::load(Arc::clone(&db)));

        // Listen for the change event (mirrors capture.rs apply_paused test).
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_listener = Arc::clone(&fired);
        app.listen(ALERT_CONFIG_CHANGED_EVENT, move |_event| {
            fired_listener.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let mut config = AlertConfig::default();
        config.delta.enabled = true;
        config.delta.step_usd = 20.0;

        let saved = alert_config_set(app.handle().clone(), config.clone()).expect("set");
        assert_eq!(saved, config);
        // Managed cache reflects the save.
        assert_eq!(app.state::<AlertState>().config(), config);
        // Persisted: the raw meta row holds the serialized config.
        assert!(read_meta(&db, ALERT_CONFIG_KEY).is_some());
        // A fresh load sees it.
        assert_eq!(AlertState::load(db).config(), config);
        // The change event fired.
        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            "alert_config_set must emit ALERT_CONFIG_CHANGED"
        );
    }

    #[test]
    fn alert_config_get_returns_managed_config() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(AlertState::load(test_db(&dir)));

        let config = alert_config_get(app.state::<AlertState>());
        // Defaults surface through the command.
        assert!(config.burst.enabled);
        assert_eq!(config.delta.step_usd, 50.0);
    }
}
