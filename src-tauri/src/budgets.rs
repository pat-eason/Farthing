//! Budget configuration (standalone build).
//!
//! Daily/monthly spend budgets plus a tray-visibility toggle, persisted as a
//! single JSON row in the `meta` table under [`BUDGET_CONFIG_KEY`]. Mirrors
//! the [`crate::capture::CaptureState`] pattern: a managed struct holding the
//! parsed config behind a mutex plus the shared DB handle, loaded once on
//! startup (defaulting on any read/parse error, never failing startup).
//!
//! The cost-notifications work that will eventually consume `notify` and
//! `approach_pct` is deferred; those fields are persisted but unused here.
//! Setting config persists first, then updates memory, then refreshes the
//! tray title and emits [`BUDGET_CONFIG_CHANGED`] for the frontend.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, Runtime};

use crate::db::{Db, DbState};
use crate::metrics;

/// `meta` key holding the persisted budget config JSON.
pub const BUDGET_CONFIG_KEY: &str = "budget_config";

/// Event emitted to the frontend whenever budget config changes; payload is
/// the resulting (clamped) [`BudgetConfig`].
pub const BUDGET_CONFIG_CHANGED: &str = "budget:config-changed";

const DEFAULT_APPROACH_PCT: f64 = 76.0;

fn default_amount_usd() -> f64 {
    0.0
}

fn default_enabled() -> bool {
    false
}

fn default_notify() -> bool {
    true
}

fn default_show_in_tray() -> bool {
    true
}

fn default_approach_pct() -> f64 {
    DEFAULT_APPROACH_PCT
}

/// One budget threshold (daily or monthly). `notify` is persisted for the
/// deferred cost-notifications work and unused here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetAmount {
    #[serde(default = "default_amount_usd")]
    pub amount_usd: f64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_notify")]
    pub notify: bool,
}

impl Default for BudgetAmount {
    fn default() -> Self {
        Self {
            amount_usd: default_amount_usd(),
            enabled: default_enabled(),
            notify: default_notify(),
        }
    }
}

/// Full budget configuration. `approach_pct` is persisted for the deferred
/// cost-notifications work and unused here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub daily: BudgetAmount,
    pub monthly: BudgetAmount,
    #[serde(default = "default_show_in_tray")]
    pub show_in_tray: bool,
    #[serde(default = "default_approach_pct")]
    pub approach_pct: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily: BudgetAmount::default(),
            monthly: BudgetAmount::default(),
            show_in_tray: default_show_in_tray(),
            approach_pct: default_approach_pct(),
        }
    }
}

/// Shared budget config: the parsed config behind a mutex plus the database
/// handle used to persist changes. Managed in the Tauri app.
#[derive(Clone)]
pub struct BudgetState {
    config: Arc<Mutex<BudgetConfig>>,
    db: Arc<Mutex<Db>>,
}

impl BudgetState {
    /// Read + parse the persisted config (default on any error) and wrap it
    /// for sharing. A missing, partial, or malformed row never fails startup.
    pub fn load(db: Arc<Mutex<Db>>) -> Self {
        let config = read_persisted(&db);
        Self {
            config: Arc::new(Mutex::new(config)),
            db,
        }
    }

    /// Current config (cloned out of the mutex).
    pub fn config(&self) -> BudgetConfig {
        self.config.lock().expect("budget mutex poisoned").clone()
    }

    /// Persist `config` (after clamping enabled amounts to >= $1.00) then
    /// update the in-memory copy. The write happens first: if it fails, the
    /// in-memory state is left unchanged so UI and disk never disagree.
    /// Returns the clamped config that was stored.
    pub fn set(&self, mut config: BudgetConfig) -> rusqlite::Result<BudgetConfig> {
        clamp(&mut config.daily);
        clamp(&mut config.monthly);

        let json = serde_json::to_string(&config)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        {
            let db = self.db.lock().expect("db mutex poisoned");
            db.conn().execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                rusqlite::params![BUDGET_CONFIG_KEY, json],
            )?;
        }
        *self.config.lock().expect("budget mutex poisoned") = config.clone();
        Ok(config)
    }
}

/// Enabled budgets must be at least $1.00; a disabled amount is left as-is.
fn clamp(amount: &mut BudgetAmount) {
    if amount.enabled && amount.amount_usd < 1.0 {
        amount.amount_usd = 1.0;
    }
}

fn read_persisted(db: &Mutex<Db>) -> BudgetConfig {
    let db = db.lock().expect("db mutex poisoned");
    db.conn()
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [BUDGET_CONFIG_KEY],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| serde_json::from_str::<BudgetConfig>(&value).ok())
        .unwrap_or_default()
}

/// Spend band for a budget line, ordered Green < Yellow < Amber < Red. The
/// `Ord` derive uses this declaration order, so `max` picks the worst band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Green,
    Yellow,
    Amber,
    Red,
}

impl Band {
    /// Band for a rounded percent: contiguous thresholds Green <=50,
    /// Yellow (50, 75], Amber (75, 90], Red > 90.
    fn from_percent(percent: i64) -> Self {
        if percent <= 50 {
            Band::Green
        } else if percent <= 75 {
            Band::Yellow
        } else if percent <= 90 {
            Band::Amber
        } else {
            Band::Red
        }
    }
}

/// One budget's current state (daily or monthly).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BudgetLine {
    pub amount_usd: f64,
    pub spent_priced_usd: f64,
    pub unpriced_requests: i64,
    /// Rounded percent of the budget spent.
    pub percent: i64,
    pub band: Band,
    /// `spent_priced_usd >= amount_usd`.
    pub exceeded: bool,
}

/// Budget status for the tray/desktop readouts. A line is `None` when its
/// budget is unset/disabled. `worst_band` is the max band across set budgets
/// (Green when none are set).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BudgetStatus {
    pub daily: Option<BudgetLine>,
    pub monthly: Option<BudgetLine>,
    pub show_in_tray: bool,
    pub worst_band: Band,
}

/// Build one budget line from priced spend over `[start, end)` (true total;
/// no event-time floor, so backfill is included).
fn line(db: &Db, amount: f64, start: i64, end: i64) -> rusqlite::Result<BudgetLine> {
    let (spent, unpriced) = metrics::priced_spend_for_window(db, start, end, None)?;
    let percent = if amount > 0.0 {
        (100.0 * spent / amount).round() as i64
    } else {
        0
    };
    Ok(BudgetLine {
        amount_usd: amount,
        spent_priced_usd: spent,
        unpriced_requests: unpriced,
        percent,
        band: Band::from_percent(percent),
        exceeded: spent >= amount,
    })
}

/// Pure budget evaluation: spend each enabled budget against its current local
/// window (day / month) and roll up the worst band. Disabled budgets yield
/// `None` lines and do not contribute to `worst_band`. The `budget_status`
/// command is a thin wrapper over this.
pub fn evaluate(
    db: &Db,
    config: &BudgetConfig,
    now: chrono::DateTime<chrono::Local>,
) -> rusqlite::Result<BudgetStatus> {
    let daily = if config.daily.enabled {
        let (start, end) = metrics::local_day_window(now);
        Some(line(db, config.daily.amount_usd, start, end)?)
    } else {
        None
    };
    let monthly = if config.monthly.enabled {
        let (start, end) = metrics::local_month_window(now);
        Some(line(db, config.monthly.amount_usd, start, end)?)
    } else {
        None
    };

    let worst_band = [daily.as_ref(), monthly.as_ref()]
        .into_iter()
        .flatten()
        .map(|l| l.band)
        .max()
        .unwrap_or(Band::Green);

    Ok(BudgetStatus {
        daily,
        monthly,
        show_in_tray: config.show_in_tray,
        worst_band,
    })
}

/// Frontend query: current budget config (settings view on open).
#[tauri::command]
pub fn budget_config_get(state: tauri::State<'_, BudgetState>) -> BudgetConfig {
    state.config()
}

/// Frontend query: current budget status (percent, band, worst-state) against
/// live spend. Reads the managed config + DB; visual total includes backfill.
#[tauri::command]
pub fn budget_status<R: Runtime>(app: tauri::AppHandle<R>) -> Result<BudgetStatus, String> {
    let config = app.state::<BudgetState>().config();
    let db = app.state::<DbState>();
    let db = db.0.lock().expect("db mutex poisoned");
    evaluate(&db, &config, chrono::Local::now())
        .map_err(|err| format!("cannot query budget status: {err}"))
}

/// Frontend action: persist budget config. Clamps enabled amounts, refreshes
/// the tray title (so the tray budget readout updates), and emits
/// [`BUDGET_CONFIG_CHANGED`]. Returns the clamped config.
#[tauri::command]
pub fn budget_config_set<R: Runtime>(
    app: tauri::AppHandle<R>,
    config: BudgetConfig,
) -> Result<BudgetConfig, String> {
    let state = app.state::<BudgetState>();
    let resulting = state
        .set(config)
        .map_err(|err| format!("cannot persist budget config: {err}"))?;
    // Tray title may show a budget readout; refresh tolerates a missing tray.
    crate::tray_title::refresh(&app);
    let _ = app.emit(BUDGET_CONFIG_CHANGED, &resulting);
    Ok(resulting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db(dir: &tempfile::TempDir) -> Arc<Mutex<Db>> {
        Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()))
    }

    #[test]
    fn defaults_on_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let state = BudgetState::load(test_db(&dir));
        let config = state.config();
        assert!(!config.daily.enabled);
        assert!(!config.monthly.enabled);
        assert!(config.daily.notify);
        assert!(config.monthly.notify);
        assert!(config.show_in_tray);
        assert_eq!(config.approach_pct, 76.0);
    }

    #[test]
    fn set_then_reload_round_trips_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        {
            let state = BudgetState::load(test_db(&dir));
            state
                .set(BudgetConfig {
                    daily: BudgetAmount {
                        amount_usd: 25.0,
                        enabled: true,
                        notify: false,
                    },
                    monthly: BudgetAmount {
                        amount_usd: 400.0,
                        enabled: true,
                        notify: true,
                    },
                    show_in_tray: false,
                    approach_pct: 80.0,
                })
                .unwrap();
        }
        let reloaded = BudgetState::load(test_db(&dir)).config();
        assert_eq!(reloaded.daily.amount_usd, 25.0);
        assert!(reloaded.daily.enabled);
        assert!(!reloaded.daily.notify);
        assert_eq!(reloaded.monthly.amount_usd, 400.0);
        assert!(reloaded.monthly.enabled);
        assert!(reloaded.monthly.notify);
        assert!(!reloaded.show_in_tray);
        assert_eq!(reloaded.approach_pct, 80.0);
    }

    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        {
            let guard = db.lock().unwrap();
            guard
                .conn()
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    rusqlite::params![BUDGET_CONFIG_KEY, "{not valid json"],
                )
                .unwrap();
        }
        let config = BudgetState::load(db).config();
        assert_eq!(config, BudgetConfig::default());
    }

    #[test]
    fn partial_json_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        {
            let guard = db.lock().unwrap();
            guard
                .conn()
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    // only daily.amount_usd present; everything else defaults
                    rusqlite::params![BUDGET_CONFIG_KEY, r#"{"daily":{"amount_usd":12.0}}"#],
                )
                .unwrap();
        }
        let config = BudgetState::load(db).config();
        assert_eq!(config.daily.amount_usd, 12.0);
        assert!(!config.daily.enabled);
        assert!(config.daily.notify);
        assert!(config.show_in_tray);
        assert_eq!(config.approach_pct, 76.0);
    }

    #[test]
    fn enabled_amount_below_one_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let state = BudgetState::load(test_db(&dir));
        let result = state
            .set(BudgetConfig {
                daily: BudgetAmount {
                    amount_usd: 0.5,
                    enabled: true,
                    notify: true,
                },
                monthly: BudgetAmount {
                    // disabled below 1.0: NOT clamped
                    amount_usd: 0.25,
                    enabled: false,
                    notify: true,
                },
                show_in_tray: true,
                approach_pct: 76.0,
            })
            .unwrap();
        assert_eq!(result.daily.amount_usd, 1.0, "enabled clamped to 1.0");
        assert_eq!(result.monthly.amount_usd, 0.25, "disabled not clamped");
    }

    #[test]
    fn config_set_command_persists_and_returns_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(crate::db::DbState(Arc::clone(&db)));
        app.manage(BudgetState::load(Arc::clone(&db)));

        let config = BudgetConfig {
            daily: BudgetAmount {
                amount_usd: 0.0,
                enabled: true,
                notify: true,
            },
            ..BudgetConfig::default()
        };
        let result = budget_config_set(app.handle().clone(), config).expect("set");
        assert_eq!(result.daily.amount_usd, 1.0);
        assert!(result.daily.enabled);

        // Persisted: a fresh load from the same database sees it.
        let reloaded = BudgetState::load(db).config();
        assert_eq!(reloaded.daily.amount_usd, 1.0);
        assert!(reloaded.daily.enabled);
    }

    #[test]
    fn config_serializes_snake_case_for_frontend() {
        let json = serde_json::to_value(BudgetConfig::default()).unwrap();
        assert_eq!(json["show_in_tray"], serde_json::json!(true));
        assert_eq!(json["approach_pct"], serde_json::json!(76.0));
        assert_eq!(json["daily"]["amount_usd"], serde_json::json!(0.0));
        assert_eq!(json["daily"]["enabled"], serde_json::json!(false));
        assert_eq!(json["daily"]["notify"], serde_json::json!(true));
    }

    // ---- budget status (unit 2) ----

    /// Open a bare DB in a temp dir and return both so the dir outlives it.
    fn status_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (dir, db)
    }

    /// Insert one priced `api_request` at `now` (always inside the current
    /// local day and month, so both windows see it).
    fn insert_priced(db: &Db, cost_usd: f64) {
        let now_ms = chrono::Local::now().timestamp_millis();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type)
                 VALUES ('s', ?1, ?2, 'api_request')",
                rusqlite::params![now_ms, cost_usd],
            )
            .unwrap();
    }

    fn daily_only(amount_usd: f64) -> BudgetConfig {
        BudgetConfig {
            daily: BudgetAmount {
                amount_usd,
                enabled: true,
                notify: true,
            },
            monthly: BudgetAmount::default(),
            show_in_tray: true,
            approach_pct: 76.0,
        }
    }

    #[test]
    fn band_thresholds_are_contiguous() {
        assert_eq!(Band::from_percent(0), Band::Green);
        assert_eq!(Band::from_percent(50), Band::Green);
        assert_eq!(Band::from_percent(51), Band::Yellow);
        assert_eq!(Band::from_percent(75), Band::Yellow);
        assert_eq!(Band::from_percent(76), Band::Amber);
        assert_eq!(Band::from_percent(90), Band::Amber);
        assert_eq!(Band::from_percent(91), Band::Red);
        assert_eq!(Band::from_percent(100), Band::Red);
        assert_eq!(Band::from_percent(150), Band::Red);
    }

    #[test]
    fn band_ordering_picks_worst() {
        assert!(Band::Red > Band::Amber);
        assert!(Band::Amber > Band::Yellow);
        assert!(Band::Yellow > Band::Green);
        assert_eq!(
            [Band::Green, Band::Amber, Band::Yellow].iter().max(),
            Some(&Band::Amber)
        );
    }

    #[test]
    fn daily_75_percent_is_yellow_not_exceeded() {
        let (_dir, db) = status_db();
        insert_priced(&db, 11.25);
        let status = evaluate(&db, &daily_only(15.0), chrono::Local::now()).unwrap();
        let line = status.daily.expect("daily set");
        assert_eq!(line.percent, 75);
        assert_eq!(line.band, Band::Yellow);
        assert!(!line.exceeded);
        assert_eq!(line.spent_priced_usd, 11.25);
        assert_eq!(status.worst_band, Band::Yellow);
    }

    #[test]
    fn band_boundaries_via_spend() {
        // amount 100 => spend == rounded percent dollars.
        let cases = [
            (50.0, 50, Band::Green, false),
            (51.0, 51, Band::Yellow, false),
            (76.0, 76, Band::Amber, false),
            (91.0, 91, Band::Red, false),
            (100.0, 100, Band::Red, true),
            (110.0, 110, Band::Red, true),
        ];
        for (spend, percent, band, exceeded) in cases {
            let (_dir, db) = status_db();
            insert_priced(&db, spend);
            let status = evaluate(&db, &daily_only(100.0), chrono::Local::now()).unwrap();
            let line = status.daily.unwrap();
            assert_eq!(line.percent, percent, "spend {spend}");
            assert_eq!(line.band, band, "spend {spend}");
            assert_eq!(line.exceeded, exceeded, "spend {spend}");
        }
    }

    #[test]
    fn null_cost_rows_excluded_and_counted_as_unpriced() {
        let (_dir, db) = status_db();
        insert_priced(&db, 5.0);
        // NULL-cost api_request: excluded from spend, counted unpriced.
        let now_ms = chrono::Local::now().timestamp_millis();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type)
                 VALUES ('s', ?1, NULL, 'api_request')",
                rusqlite::params![now_ms],
            )
            .unwrap();
        let status = evaluate(&db, &daily_only(100.0), chrono::Local::now()).unwrap();
        let line = status.daily.unwrap();
        assert_eq!(line.spent_priced_usd, 5.0);
        assert_eq!(line.unpriced_requests, 1);
    }

    #[test]
    fn only_monthly_enabled_yields_no_daily_line() {
        let (_dir, db) = status_db();
        insert_priced(&db, 80.0);
        let config = BudgetConfig {
            daily: BudgetAmount::default(), // disabled
            monthly: BudgetAmount {
                amount_usd: 100.0,
                enabled: true,
                notify: true,
            },
            show_in_tray: true,
            approach_pct: 76.0,
        };
        let status = evaluate(&db, &config, chrono::Local::now()).unwrap();
        assert!(status.daily.is_none());
        let monthly = status.monthly.expect("monthly set");
        assert_eq!(monthly.percent, 80);
        assert_eq!(monthly.band, Band::Amber);
        assert_eq!(status.worst_band, Band::Amber, "worst from monthly alone");
    }

    #[test]
    fn no_budgets_set_is_green() {
        let (_dir, db) = status_db();
        insert_priced(&db, 999.0);
        let status = evaluate(&db, &BudgetConfig::default(), chrono::Local::now()).unwrap();
        assert!(status.daily.is_none());
        assert!(status.monthly.is_none());
        assert_eq!(status.worst_band, Band::Green);
        assert!(status.show_in_tray);
    }

    #[test]
    fn backfilled_past_dated_rows_in_window_are_included() {
        let (_dir, db) = status_db();
        // A row earlier today (still inside the local-day window) with no
        // event-time floor: it must count toward the true total.
        let earlier_ms = chrono::Local::now().timestamp_millis() - 60_000;
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type, source)
                 VALUES ('s', ?1, 30.0, 'api_request', 'backfill')",
                rusqlite::params![earlier_ms],
            )
            .unwrap();
        insert_priced(&db, 20.0);
        let status = evaluate(&db, &daily_only(100.0), chrono::Local::now()).unwrap();
        let line = status.daily.unwrap();
        assert_eq!(line.spent_priced_usd, 50.0, "backfill included (no floor)");
        assert_eq!(line.percent, 50);
        assert_eq!(line.band, Band::Green);
    }

    #[test]
    fn status_serializes_lowercase_band_for_frontend() {
        let (_dir, db) = status_db();
        insert_priced(&db, 95.0);
        let status = evaluate(&db, &daily_only(100.0), chrono::Local::now()).unwrap();
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["worst_band"], serde_json::json!("red"));
        assert_eq!(value["daily"]["band"], serde_json::json!("red"));
        assert_eq!(value["daily"]["exceeded"], serde_json::json!(false));
        assert_eq!(value["monthly"], serde_json::json!(null));
        assert_eq!(value["show_in_tray"], serde_json::json!(true));
    }

    #[test]
    fn status_command_reads_managed_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()));
        {
            let guard = db.lock().unwrap();
            let now_ms = chrono::Local::now().timestamp_millis();
            guard
                .conn()
                .execute(
                    "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type)
                     VALUES ('s', ?1, 7.5, 'api_request')",
                    rusqlite::params![now_ms],
                )
                .unwrap();
        }
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DbState(Arc::clone(&db)));
        let state = BudgetState::load(Arc::clone(&db));
        state.set(daily_only(15.0)).unwrap();
        app.manage(state);

        let status = budget_status(app.handle().clone()).expect("status");
        let line = status.daily.expect("daily set");
        assert_eq!(line.spent_priced_usd, 7.5);
        assert_eq!(line.percent, 50);
        assert_eq!(line.band, Band::Green);
    }
}
