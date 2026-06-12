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

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::db::{Db, DbState};

/// Minimum gap between ingest-triggered evaluations (ms). A runaway agent loop
/// emits many OTLP exports per second; evaluating on every one would serialize
/// a windowed spend query behind every ingest write. Coalescing to this floor
/// holds the "alert within ~1 min" budget while bounding contention — see the
/// plan's debounce decision. The 60s tick and config-save evals are *not*
/// debounced (they are already rare and must always run).
const INGEST_EVAL_DEBOUNCE_MS: i64 = 5_000;

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
    /// Unix ms of the last ingest-triggered evaluation, for the debounce. Only
    /// the ingest path consults it (via [`AlertState::should_run_ingest_eval`]);
    /// the tick and config-save paths bypass it. `0` means "never run", so the
    /// first ingest after launch always evaluates.
    last_ingest_eval_ms: Arc<AtomicI64>,
    /// In-flight guard for the `spawn_blocking` eval tasks: `true` while a
    /// `gather_and_apply` task is queued or running on the thread pool. Prevents
    /// unbounded task accumulation under eval-lock contention (e.g. during a
    /// backfill pass). Swapped `false → true` before spawning and cleared at the
    /// end of the task (even on panic, via a drop-guard). A `compare_exchange`
    /// failure means "already in flight; skip this spawn". The debounce gate
    /// (frequency) and this flag (concurrency) are complementary concerns.
    eval_in_flight: Arc<AtomicBool>,
}

impl AlertState {
    /// Read the persisted config + runtime (defaults on absent/malformed JSON)
    /// and capture `process_start_ms` from the current wall clock. A bad `meta`
    /// row never fails startup, mirroring [`CaptureState::load`].
    pub fn load(db: Arc<Mutex<Db>>) -> Self {
        let config: AlertConfig = sanitize_config(read_json(&db, ALERT_CONFIG_KEY));
        let runtime = read_json(&db, ALERT_RUNTIME_KEY);
        Self {
            db,
            config: Arc::new(Mutex::new(config)),
            runtime: Arc::new(Mutex::new(runtime)),
            process_start_ms: chrono::Local::now().timestamp_millis(),
            eval_lock: Arc::new(Mutex::new(())),
            last_ingest_eval_ms: Arc::new(AtomicI64::new(0)),
            eval_in_flight: Arc::new(AtomicBool::new(false)),
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
    ///
    /// **Poison recovery**: unlike the DB mutex (which guards real state that a
    /// panic could leave inconsistent), the eval lock guards a *critical section*
    /// — its protected value is `()`, so there is no invariant to uphold and no
    /// corruption to carry forward. Recovering from a poison lets the next
    /// evaluation cycle proceed normally instead of permanently silencing alerts.
    /// The DB mutex in contrast keeps its `.expect` because a panicked write
    /// half-way through a statement could leave the DB in a bad state.
    pub fn eval_guard(&self) -> MutexGuard<'_, ()> {
        self.eval_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Try to claim the in-flight slot for a spawned eval task. Returns `true`
    /// and marks the slot taken when no task is already in flight; returns
    /// `false` (don't spawn) when one is. The caller MUST call
    /// [`end_eval`](Self::end_eval) after the task finishes, even on panic
    /// (use an [`EvalGuard`] drop-guard).
    pub fn try_begin_eval(&self) -> bool {
        self.eval_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the in-flight slot. Called at the end of a spawned eval task
    /// (via [`EvalGuard`] drop so panics don't leave the slot permanently set).
    pub fn end_eval(&self) {
        self.eval_in_flight.store(false, Ordering::SeqCst);
    }

    /// Debounce gate for the ingest path: return `true` (and claim the slot) iff
    /// at least [`INGEST_EVAL_DEBOUNCE_MS`] have elapsed since the last claimed
    /// ingest evaluation. Atomic claim-and-update so two near-simultaneous ingest
    /// callbacks can't both pass the gate; a `false` return means "skip, a recent
    /// eval already covered this burst of exports". Called *before* taking the
    /// eval lock so a debounced-out ingest never even queues behind the lock.
    pub fn should_run_ingest_eval(&self, now_ms: i64) -> bool {
        let last = self.last_ingest_eval_ms.load(Ordering::SeqCst);
        if now_ms - last < INGEST_EVAL_DEBOUNCE_MS {
            return false;
        }
        // Claim the slot. A racing caller that read the same `last` and lost the
        // CAS sees the other's write and (re-checking) debounces out, so at most
        // one of a simultaneous pair proceeds.
        self.last_ingest_eval_ms
            .compare_exchange(last, now_ms, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Current config (cheap clone of the cached value; no DB read).
    pub fn config(&self) -> AlertConfig {
        self.config
            .lock()
            .expect("alert config mutex poisoned")
            .clone()
    }

    /// Current runtime (cheap clone of the cached value; no DB read).
    pub fn runtime(&self) -> AlertRuntime {
        self.runtime
            .lock()
            .expect("alert runtime mutex poisoned")
            .clone()
    }

    /// Persist `config` then update the cache. Write-first (like
    /// [`CaptureState::set_paused`]): if the DB write fails the in-memory cache is
    /// left unchanged so disk and memory never disagree. The config is sanitized
    /// before persist so the on-disk value is always clamped (matching the
    /// UI-level validation).
    pub fn set_config(&self, config: AlertConfig) -> Result<(), rusqlite::Error> {
        let config = sanitize_config(config);
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
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
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

/// Clamp `config` to valid ranges, mirroring the UI-level validation. Guards
/// against corrupt or hand-edited `meta` rows producing pathological behaviour:
/// - `burst.cooldown_minutes = 0` would make burst fire every tick with no
///   suppression gap, potentially spamming the OS notification center
/// - `burst.window_minutes = 0` would produce a zero-length window sum (always
///   zero), so burst could never fire at all
/// - `burst.threshold_usd <= 0` or `delta.step_usd <= 0` would cause division
///   by zero or infinite step ladders in `delta_step`
///
/// The UI enforces `window_minutes >= 1`, `cooldown_minutes >= 1`,
/// `threshold_usd > 0`, and `step_usd > 0`; this function applies the same
/// constraints so the backend is independently correct regardless of how the
/// config row was written.
fn sanitize_config(mut config: AlertConfig) -> AlertConfig {
    if config.burst.window_minutes < 1 {
        config.burst.window_minutes = default_burst_window_minutes();
    }
    if config.burst.cooldown_minutes < 1 {
        config.burst.cooldown_minutes = default_burst_cooldown_minutes();
    }
    if config.burst.threshold_usd <= 0.0 {
        config.burst.threshold_usd = default_burst_threshold_usd();
    }
    if config.delta.step_usd <= 0.0 {
        config.delta.step_usd = default_delta_step_usd();
    }
    config
}

/// RAII guard that clears the eval in-flight flag when dropped, so a panic
/// inside a spawned `gather_and_apply` task cannot permanently block future
/// evaluations. Construct via [`AlertState::try_begin_eval`].
pub struct EvalGuard(AlertState);

impl Drop for EvalGuard {
    fn drop(&mut self) {
        self.0.end_eval();
    }
}

/// Frontend query: the current alert config (Spend UI reads this on mount).
#[tauri::command]
pub fn alert_config_get(state: tauri::State<'_, AlertState>) -> AlertConfig {
    state.config()
}

/// Frontend query: the current alert runtime (Spend UI reads this for cooldown
/// and permission-lost state on mount and after config changes). Returns a
/// cheap clone of the in-memory cache; no DB read.
#[tauri::command]
pub fn alert_runtime_get(state: tauri::State<'_, AlertState>) -> AlertRuntime {
    state.runtime()
}

/// Frontend action: persist a new alert config, re-evaluate, and notify the UI.
///
/// Persists the config and emits [`ALERT_CONFIG_CHANGED_EVENT`] synchronously
/// on the IPC thread (these are cheap, always safe). The re-evaluation is
/// offloaded to a `spawn_blocking` task so the IPC thread is never stalled by
/// the eval lock or DB queries — the same pattern the 60s tick and ingest path
/// use. The re-eval runs promptly (the task is submitted immediately) but does
/// not block the caller's return.
///
/// Why offload and not inline? `alert_config_set` runs on the Tauri IPC thread;
/// `gather_and_apply` takes the eval lock and runs two DB queries plus a write.
/// Under backfill the eval lock can be held for the duration of the pass; an
/// inline call would stall the IPC thread for the same duration, making the UI
/// unresponsive. Offloading keeps the "fires immediately on config save"
/// guarantee (the task is submitted before we return) without the stall risk.
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
    let _ = app.emit(ALERT_CONFIG_CHANGED_EVENT, &saved);
    // Offload re-eval so this IPC call returns immediately even under eval-lock
    // contention. The task is submitted now so the immediate-effect intent is
    // preserved: a tightened threshold or just-enabled rule can fire right away.
    reevaluate_after_config_change(&app);
    Ok(saved)
}

// ---- evaluation engine (Unit 4) ----
//
// The decision core is a *pure* function: [`evaluate`] takes the current instant,
// the config, the prior runtime, and the already-queried spend sums, and returns
// the notifications to show plus the runtime to persist. It touches no clock and
// no database — the live-vs-historical discrimination lives in the query assembly
// (the `process_start_ms` event-time floor passed into the Unit 2 priced-spend
// query), *not* here, so `evaluate` simply consumes the already-floored sums and
// stays deterministically testable. The helpers it leans on (`month_key`,
// `in_quiet_hours`, the delta step math) are the shared primitives the Budgets
// plan reuses for its approach/breach groups.

/// The spend inputs [`evaluate`] consumes, already shaped by the query assembly
/// (Unit 5 fills these from the Unit 2 priced-spend queries).
///
/// Both sums are *priced-only* (NULL-cost rows excluded, per `docs/notes/pricing.md`)
/// and *event-time floored at `process_start_ms`* by the query, so spend recovered
/// from before this process launched can never reach the engine. `evaluate` does
/// not re-derive that floor; it trusts the caller assembled the windows correctly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sums {
    /// Priced spend inside the burst rolling window, floored at
    /// `max(now - window, process_start_ms)`. Compared against `threshold_usd`.
    pub burst_window_priced_sum: f64,
    /// Priced month-to-date spend counting only post-launch rows
    /// (floored at `process_start_ms`). Drives the delta step ladder.
    pub post_launch_priced_mtd: f64,
}

/// Which rule produced a [`Notification`]; mirrors the `rule_type` vocabulary in
/// [`crate::notify`] so Unit 5 can route copy/test parity off one tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RuleType {
    /// The session/burst rate alert.
    Burst,
    /// The recurring-delta milestone alert.
    Delta,
}

/// A decision the engine made to alert: the title/body Unit 5 hands to
/// [`crate::notify::show`], tagged with the rule that produced it. `evaluate`
/// builds these; it never delivers them (no clock, no OS, no DB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub rule_type: RuleType,
    pub title: String,
    pub body: String,
}

/// Wall-clock month key for `now_local`, e.g. `"2026-06"`. Derived from local
/// time so a month rollover is observed the instant local midnight on the first
/// passes, never from a tick count. The delta baseline keys on this so steps
/// never carry across calendar months.
pub fn month_key(now_local: chrono::DateTime<chrono::Local>) -> String {
    now_local.format("%Y-%m").to_string()
}

/// Whether `now_local` falls inside a quiet-hours window, wrap-aware.
///
/// Membership is computed in *local* minutes-since-midnight (quiet hours are a
/// wall-clock concept; cooldown durations are UTC ms and handled separately).
/// A same-day window (`start <= end`) is the half-open interval `[start, end)`;
/// an overnight window (`start > end`, e.g. 22:00–07:00) is `[start, 24:00) ∪
/// [00:00, end)`. A degenerate `start == end` is treated as "unset" → never
/// quiet (the UI never stores such a window, but a hand-edited config might).
/// An unparseable `"HH:MM"` end is treated as not-quiet rather than panicking.
pub fn in_quiet_hours(now_local: chrono::DateTime<chrono::Local>, quiet: &QuietWindow) -> bool {
    let (Some(start), Some(end)) = (parse_hhmm(&quiet.start), parse_hhmm(&quiet.end)) else {
        return false;
    };
    if start == end {
        return false; // unset
    }
    let t = local_minutes(now_local);
    if start < end {
        (start..end).contains(&t)
    } else {
        t >= start || t < end
    }
}

/// Parse `"HH:MM"` 24-hour local time to minutes-since-midnight (`0..=1439`).
/// Returns `None` on anything malformed so a bad config row degrades to
/// not-quiet rather than panicking on parse.
fn parse_hhmm(value: &str) -> Option<u32> {
    let (h, m) = value.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    Some(h * 60 + m)
}

/// Minutes since local midnight for `now_local` (`0..=1439`).
fn local_minutes(now_local: chrono::DateTime<chrono::Local>) -> u32 {
    use chrono::Timelike;
    now_local.hour() * 60 + now_local.minute()
}

/// Highest milestone step crossed by `mtd` at `step_usd` increments:
/// `floor(mtd / step_usd)`. A non-positive step (disabled/garbage config) yields
/// `0` so the ladder never divides by zero or runs away.
fn delta_step(mtd: f64, step_usd: f64) -> i64 {
    if step_usd <= 0.0 || mtd <= 0.0 {
        return 0;
    }
    (mtd / step_usd).floor() as i64
}

/// The pure decision core: given the current instant, the config, the prior
/// runtime, and the already-floored spend sums, return the notifications to show
/// and the runtime to persist.
///
/// `now_local` carries both halves the engine needs: `now_local.timestamp_millis()`
/// is the UTC-ms instant used for cooldown arithmetic (DST-safe — a fall-back
/// repeated local hour does not rewind monotonic UTC ms), and the local wall-clock
/// fields drive quiet-hours membership and the month key. No other clock is read.
///
/// **Delta.** Re-baselines on a month rollover (the stored `month_key` differs
/// from the current one): `last_step` resets to the step the current MTD already
/// sits at, so the new month never replays the prior month's milestones. Within a
/// month it fires once per newly-crossed step (`step > last_step`), and always
/// advances `last_step` to the new step — even when quiet hours suppress the
/// *notification* — so a suppressed milestone is dropped silently and never
/// re-fires later (R17, no pending flag). A pre-bumped `last_step` (Unit 5's
/// backfill re-baseline) is honored verbatim, so only post-bump live growth fires
/// (C6/C7).
///
/// **Burst.** Fires when the floored rolling-window sum meets the threshold, the
/// cooldown has elapsed (`now_ms >= cooldown_until_ms`), and it is not quiet. A
/// fire arms the cooldown (`cooldown_until_ms = now_ms + cooldown`); a suppressed
/// (quiet) or cooled-down crossing fires nothing and leaves the cooldown intact.
pub fn evaluate(
    now_local: chrono::DateTime<chrono::Local>,
    config: &AlertConfig,
    runtime: &AlertRuntime,
    sums: Sums,
) -> (Vec<Notification>, AlertRuntime) {
    let now_ms = now_local.timestamp_millis();
    let mut notifications = Vec::new();
    let mut next = runtime.clone();

    // ---- delta ----
    let current_month = month_key(now_local);
    let current_step = delta_step(sums.post_launch_priced_mtd, config.delta.step_usd);
    if next.delta.month_key != current_month {
        // Month rollover (or first-ever evaluation): re-baseline to where MTD
        // already sits so the new month never replays passed milestones. No fire.
        next.delta.month_key = current_month;
        next.delta.last_step = current_step;
    } else if config.delta.enabled && current_step > next.delta.last_step {
        // A new milestone crossed by live, post-launch growth. Quiet hours
        // suppress the notification but the baseline still advances, so the
        // suppressed step is dropped silently and cannot re-fire later.
        let quiet = config
            .delta
            .quiet
            .as_ref()
            .is_some_and(|q| in_quiet_hours(now_local, q));
        if !quiet {
            notifications.push(delta_notification(config, current_step));
        }
        next.delta.last_step = current_step;
    }

    // ---- burst ----
    if config.burst.enabled
        && sums.burst_window_priced_sum >= config.burst.threshold_usd
        && now_ms >= next.burst.cooldown_until_ms
    {
        let quiet = config
            .burst
            .quiet
            .as_ref()
            .is_some_and(|q| in_quiet_hours(now_local, q));
        if !quiet {
            notifications.push(burst_notification(config, sums.burst_window_priced_sum));
            // Arm the cooldown in UTC ms (durations never use local time).
            next.burst.cooldown_until_ms =
                now_ms + i64::from(config.burst.cooldown_minutes) * 60_000;
        }
        // A quiet crossing fires nothing and leaves the cooldown unarmed: the
        // next non-quiet crossing is free to fire (R17 drop, no pending flag).
    }

    (notifications, next)
}

/// Build the delta milestone copy. Amount = `step * step_usd` (the milestone the
/// user just crossed). Copy switches to real-money wording when `api_billing` is
/// set, neutral usage wording otherwise.
fn delta_notification(config: &AlertConfig, step: i64) -> Notification {
    let amount = step as f64 * config.delta.step_usd;
    let (title, body) = if config.api_billing {
        (
            "Spend milestone",
            format!("You've spent ${} this month", fmt_usd(amount)),
        )
    } else {
        (
            "Usage milestone",
            format!("${} of usage this month", fmt_usd(amount)),
        )
    };
    Notification {
        rule_type: RuleType::Delta,
        title: title.to_string(),
        body,
    }
}

/// Build the burst copy. `sum` is the priced spend in the rolling window that
/// armed the alert; `window_minutes` frames it. Real-money vs neutral per
/// `api_billing`.
fn burst_notification(config: &AlertConfig, sum: f64) -> Notification {
    let minutes = config.burst.window_minutes;
    let (title, body) = if config.api_billing {
        (
            "Spend spike",
            format!("${} spent in the last {minutes} minutes", fmt_usd(sum)),
        )
    } else {
        (
            "Usage spike",
            format!("${} of usage in the last {minutes} minutes", fmt_usd(sum)),
        )
    };
    Notification {
        rule_type: RuleType::Burst,
        title: title.to_string(),
        body,
    }
}

/// Format a USD amount for notification copy: whole dollars render without a
/// decimal (`$50`), fractional amounts keep two places (`$12.40`).
fn fmt_usd(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount:.2}")
    }
}

/// Re-evaluation seam invoked by [`alert_config_set`] after a config save.
///
/// Submits a `spawn_blocking` task for [`gather_and_apply`] rather than calling
/// it inline — the IPC thread must not block under the eval lock (see
/// [`alert_config_set`] docs). The task is submitted synchronously so the
/// immediate-effect intent is preserved: the eval runs as soon as the thread
/// pool has capacity, without the IPC caller waiting for it. Not debounced —
/// config saves are rare and the user expects the change to take effect now.
fn reevaluate_after_config_change<R: Runtime>(app: &AppHandle<R>) {
    spawn_eval(app);
}

/// Submit a `gather_and_apply` task on the blocking thread pool, guarded by the
/// in-flight flag. If a task is already queued or running, the spawn is skipped
/// (the in-flight eval will pick up any config/spend changes it sees on wakeup).
///
/// This is the single spawn site used by the 60s tick, ingest path, and config
/// save — centralising the in-flight guard so all three callers share one
/// concurrency cap. The debounce gate ([`AlertState::should_run_ingest_eval`])
/// is a *frequency* cap; this flag is a *concurrency* cap — complementary
/// concerns. Call this instead of `gather_and_apply` from async contexts.
pub fn spawn_eval<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AlertState>() else {
        return;
    };
    // If a task is already in flight, skip this spawn. The running task will
    // process the latest state when it wakes up under the eval lock.
    if !state.try_begin_eval() {
        return;
    }
    // The drop-guard clears the flag at the end of the task, including on panic,
    // so a panicking task cannot permanently block future evaluations.
    let guard = EvalGuard(state.inner().clone());
    let eval_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _g = guard; // dropped at end of closure (or on panic)
        gather_and_apply(&eval_app);
    });
}

// ---- runtime orchestration (Unit 5) ----
//
// `gather_and_apply` is the single place the pure engine meets the live system:
// it acquires the eval lock, queries the two spend sums (event-time floored at
// `process_start_ms` by the query, never inside `evaluate`), runs `evaluate`,
// delivers each notification through the permission-gated `notify::show`, records
// a permission-lost signal if a send was gated, and persists the returned runtime
// — all under the lock so ingest-path, 60s-tick, and config-save evals are
// mutually exclusive (no lost update, no double-fire). A failed query is logged
// and the cycle skipped; it never crashes the caller (the tick loop or ingest
// notifier must survive a transient DB error).

/// Run one evaluation cycle for `app`: gather sums, evaluate, deliver, persist.
///
/// Acquires the [`AlertState`] eval lock for the whole cycle. Queries are floored
/// at `process_start_ms` (delta MTD) and at `max(now - window, process_start_ms)`
/// (burst window) so recovered pre-launch spend can never trip a live alert. A
/// query error is logged and the cycle abandoned without touching runtime; a
/// `show` that returns [`crate::notify::ShowOutcome::PermissionDenied`] sets
/// `permission_lost` on the runtime that gets persisted. Safe to call from any
/// trigger (ingest, tick, config save); the debounce for the ingest path is the
/// caller's responsibility (see [`AlertState::should_run_ingest_eval`]).
pub fn gather_and_apply<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AlertState>() else {
        return; // not yet managed (startup ordering / tests)
    };
    let Some(db_state) = app.try_state::<DbState>() else {
        return;
    };

    // Hold the eval lock across the entire read→evaluate→persist cycle. This is
    // the atomicity guarantee: the DB mutex only serializes individual
    // statements, not the runtime read-modify-write.
    let _guard = state.eval_guard();

    let now_local = chrono::Local::now();
    let config = state.config();
    let runtime = state.runtime();

    let sums = match gather_sums(&db_state.0, &config, state.process_start_ms(), now_local) {
        Ok(sums) => sums,
        Err(err) => {
            // A transient query failure must never crash the tick/ingest path;
            // skip this cycle and let the next trigger retry.
            eprintln!("alerts: skipping evaluation, cannot query spend: {err}");
            return;
        }
    };

    let (notifications, next) = evaluate(now_local, &config, &runtime, sums);

    // Deliver through the permission-gated OS seam and fold the permission-lost
    // signal into the runtime to persist. Factored out so the gating contract
    // (a denied `show` flips `permission_lost`) is unit-testable without an OS
    // notification backend (which neither MockRuntime nor `tauri dev` provides).
    //
    // Permission state is sampled here (not in deliver_and_record) so it
    // reflects the *current* OS state on every cycle — including idle ticks
    // where no notifications are queued. This makes `permission_lost` track the
    // actual permission rather than latching permanently on the first denial.
    use tauri::plugin::PermissionState;
    use tauri_plugin_notification::NotificationExt;
    let current_permission = app
        .notification()
        .permission_state()
        .unwrap_or(PermissionState::Prompt);
    let next = deliver_and_record(&notifications, next, current_permission, |note| {
        crate::notify::show(app, &note.title, &note.body)
    });

    if let Err(err) = state.set_runtime(next) {
        // Persist failure leaves the cache unchanged (write-first); log and move
        // on. The dropped runtime advance can at worst replay one milestone next
        // cycle — acceptable versus crashing the trigger.
        eprintln!("alerts: cannot persist alert runtime: {err}");
    }
}

/// Deliver each notification through `show` and return the runtime to persist,
/// with `permission_lost` reflecting the *actual* current OS permission state
/// (`current_permission`). This keeps `permission_lost` as a live signal rather
/// than a one-way latch:
///
/// - `Granted` → `permission_lost = false`, even if it was previously `true`
///   (the user re-granted permission in System Settings)
/// - anything else → `permission_lost = true` (denied or not yet prompted)
///
/// Individual delivery outcomes are still checked: a `PermissionDenied` result
/// from `show` also sets the flag (defensive belt-and-suspenders), but the
/// *clearing* path comes from `current_permission` so an idle cycle with no
/// notifications to deliver still clears a stale flag.
///
/// The dedup state in `next` (advanced delta step, armed burst cooldown) is
/// preserved regardless of delivery outcome — a recovered permission must not
/// replay a flood of the milestones suppressed while it was off.
fn deliver_and_record(
    notifications: &[Notification],
    mut next: AlertRuntime,
    current_permission: tauri::plugin::PermissionState,
    mut show: impl FnMut(&Notification) -> crate::notify::ShowOutcome,
) -> AlertRuntime {
    // Set permission_lost based on the actual OS state sampled this cycle.
    // This is the only clearing path: `current_permission == Granted` clears a
    // previously-set flag when the user re-grants in System Settings.
    next.permission_lost = current_permission != tauri::plugin::PermissionState::Granted;
    for note in notifications {
        if show(note) == crate::notify::ShowOutcome::PermissionDenied {
            // Belt-and-suspenders: the gate also observed denial; set the flag
            // (it may already be set from current_permission above, but this
            // path is correct for defensive coverage).
            next.permission_lost = true;
        }
    }
    next
}

/// Query the two priced-only, event-time-floored spend sums [`evaluate`] needs.
///
/// - `burst_window_priced_sum`: priced spend over `[now - window, now)`, floored
///   at `max(now - window_ms, process_start_ms)` so a backfilled or otel-flipped
///   row dated before launch is excluded even when its window math would include
///   it.
/// - `post_launch_priced_mtd`: priced spend over the local calendar-month window
///   floored at `process_start_ms`, so the delta ladder counts only post-launch
///   growth (the backfill re-baseline in [`rebaseline_delta_now`] bumps the
///   stored step to match before any live eval).
fn gather_sums(
    db: &Mutex<Db>,
    config: &AlertConfig,
    process_start_ms: i64,
    now_local: chrono::DateTime<chrono::Local>,
) -> Result<Sums, rusqlite::Error> {
    let now_ms = now_local.timestamp_millis();
    let window_ms = i64::from(config.burst.window_minutes) * 60_000;
    let burst_start = now_ms - window_ms;
    let burst_floor = burst_start.max(process_start_ms);
    let (month_start, month_end) = crate::metrics::local_month_window(now_local);

    let db = db.lock().expect("db mutex poisoned");
    // Burst: the rolling window is [burst_start, now); the floor drops pre-launch
    // rows. `end` is exclusive (the [start, end) convention used everywhere), so
    // a row stamped exactly `now_ms` is excluded — fine, it's "the future" for
    // this instant's window and the next eval will include it.
    let (burst_window_priced_sum, _) =
        crate::metrics::priced_spend_for_window(&db, burst_start, now_ms, Some(burst_floor))?;
    // Delta MTD: the calendar-month window, floored at launch so recovered
    // pre-launch month spend never advances the ladder.
    let (post_launch_priced_mtd, _) = crate::metrics::priced_spend_for_window(
        &db,
        month_start,
        month_end,
        Some(process_start_ms),
    )?;

    Ok(Sums {
        burst_window_priced_sum,
        post_launch_priced_mtd,
    })
}

/// Silently re-baseline the delta ladder to the current post-launch MTD step,
/// firing nothing. Called at the two `run_pass` (backfill) call sites that hold
/// an `AppHandle`: a backfill pass can recover a large chunk of pre-launch month
/// spend, and without this the *next* live evaluation would see MTD jump past
/// many milestones and (absent the floor) flood. Even with the
/// `process_start_ms` floor on the live query, re-baselining keeps `last_step`
/// honest against the post-launch MTD so only genuine post-launch growth fires.
///
/// Runs under the eval lock (mutually exclusive with [`gather_and_apply`]). A
/// query or persist error is logged and skipped — a backfill pass must complete
/// regardless of whether the re-baseline succeeds.
pub fn rebaseline_delta_now<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AlertState>() else {
        return;
    };
    let Some(db_state) = app.try_state::<DbState>() else {
        return;
    };

    let _guard = state.eval_guard();

    let now_local = chrono::Local::now();
    let config = state.config();
    let mut runtime = state.runtime();

    let (month_start, month_end) = crate::metrics::local_month_window(now_local);
    let mtd = {
        let db = db_state.0.lock().expect("db mutex poisoned");
        match crate::metrics::priced_spend_for_window(
            &db,
            month_start,
            month_end,
            Some(state.process_start_ms()),
        ) {
            Ok((sum, _)) => sum,
            Err(err) => {
                eprintln!("alerts: cannot re-baseline delta, spend query failed: {err}");
                return;
            }
        }
    };

    // Bump silently to where MTD sits now (and key on the current month). The
    // step math is the same `evaluate` uses, so a subsequent live eval honors
    // this baseline verbatim and only fires on growth past it (C6/C7).
    runtime.delta.month_key = month_key(now_local);
    runtime.delta.last_step = delta_step(mtd, config.delta.step_usd);
    if let Err(err) = state.set_runtime(runtime) {
        eprintln!("alerts: cannot persist delta re-baseline: {err}");
    }
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

    // #4: a prior panic that poisoned the eval lock must not permanently silence
    // alerts. Because the guarded value is `()` (no invariant to uphold), the
    // lock recovers rather than propagating the panic.
    #[test]
    fn poisoned_eval_lock_still_yields_a_usable_guard() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));

        // Poison the lock by panicking inside a thread that holds it, then
        // catching the panic (std::panic::catch_unwind so the test thread lives).
        let lock = Arc::clone(&state.eval_lock);
        let _ = std::panic::catch_unwind(move || {
            let _g = lock.lock().unwrap();
            panic!("intentional poisoning");
        });
        assert!(state.eval_lock.is_poisoned(), "lock must be poisoned now");

        // eval_guard must recover from the poisoned state and return a usable guard
        // (no panic, not permanently stuck).
        let _guard = state.eval_guard(); // must not panic
                                         // The guard is exclusive: a concurrent try_lock fails while held.
        assert!(
            state.eval_lock.try_lock().is_err(),
            "recovered guard still exclusive"
        );
    }

    // #9: malformed config with pathological values (cooldown_minutes=0, etc.)
    // must be clamped to defaults, never stored as-is.
    #[test]
    fn malformed_config_with_zero_cooldown_loads_as_clamped_default() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        // Write a config with every guarded field set to a pathological value.
        {
            let conn = db.lock().unwrap();
            conn.conn()
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    rusqlite::params![
                        ALERT_CONFIG_KEY,
                        r#"{"burst":{"enabled":true,"threshold_usd":-1,"window_minutes":0,"cooldown_minutes":0},"delta":{"enabled":true,"step_usd":0}}"#
                    ],
                )
                .unwrap();
        }

        let state = AlertState::load(db);
        let config = state.config();
        assert!(
            config.burst.cooldown_minutes >= 1,
            "cooldown_minutes=0 must be clamped, got {}",
            config.burst.cooldown_minutes
        );
        assert!(
            config.burst.window_minutes >= 1,
            "window_minutes=0 must be clamped, got {}",
            config.burst.window_minutes
        );
        assert!(
            config.burst.threshold_usd > 0.0,
            "threshold_usd<=0 must be clamped, got {}",
            config.burst.threshold_usd
        );
        assert!(
            config.delta.step_usd > 0.0,
            "step_usd<=0 must be clamped, got {}",
            config.delta.step_usd
        );
    }

    // #10: alert_runtime_get command returns the current managed runtime.
    #[test]
    fn alert_runtime_get_returns_managed_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state = AlertState::load(Arc::clone(&db));

        // Write a non-default runtime so we can confirm it's the one returned.
        let runtime = AlertRuntime {
            delta: DeltaRuntime {
                month_key: "2026-06".into(),
                last_step: 5,
            },
            burst: BurstRuntime {
                cooldown_until_ms: 999_999,
            },
            permission_lost: true,
        };
        state.set_runtime(runtime.clone()).unwrap();
        app.manage(state);

        let returned = alert_runtime_get(app.state::<AlertState>());
        assert_eq!(
            returned, runtime,
            "alert_runtime_get must return the current managed runtime"
        );
    }

    // #12: try_begin_eval/end_eval: only one eval task in flight at a time.
    #[test]
    fn eval_in_flight_guard_prevents_concurrent_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));

        // First claim succeeds.
        assert!(state.try_begin_eval(), "first try_begin_eval must succeed");
        // A second attempt while in-flight is rejected.
        assert!(
            !state.try_begin_eval(),
            "second try_begin_eval must fail while in-flight"
        );
        // Releasing clears the flag.
        state.end_eval();
        // Now a fresh claim succeeds again.
        assert!(
            state.try_begin_eval(),
            "try_begin_eval must succeed after end_eval"
        );
        state.end_eval();
    }

    // #12: EvalGuard drop clears the flag even on panic, so a panicking eval
    // task cannot permanently block future evaluations.
    #[test]
    fn eval_guard_drop_clears_in_flight_flag_on_panic() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));
        assert!(state.try_begin_eval());

        // Simulate a panicking task that holds an EvalGuard.
        let guard = EvalGuard(state.clone());
        let _ = std::panic::catch_unwind(move || {
            let _g = guard;
            panic!("task panic");
        });

        // The flag must be cleared by the drop-guard's Drop impl.
        assert!(
            state.try_begin_eval(),
            "in-flight flag must be cleared after panicking task's EvalGuard drops"
        );
        state.end_eval();
    }

    // ---- evaluation engine (Unit 4) ----
    //
    // `evaluate` is pure; these table-driven cases ARE the spec (the flow-analysis
    // edge cases C6, C7, I1, R17, M2 and the cooldown/process-start scenarios).
    // Sums arrive already event-time-floored by the query assembly, so a
    // "pre-launch spend" case just means a small `burst_window_priced_sum`.

    /// A fixed local instant for cases that don't care about the wall clock
    /// (no quiet windows, mid-month so no rollover surprises). 2026-06-15 12:00.
    fn at_noon() -> chrono::DateTime<chrono::Local> {
        local_at(2026, 6, 15, 12, 0)
    }

    /// Build a `DateTime<Local>` for a wall-clock local date/time. Used so quiet
    /// and rollover cases pin the *local* fields the engine reads.
    fn local_at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone;
        let naive = chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap();
        chrono::Local.from_local_datetime(&naive).single().unwrap()
    }

    /// Delta-only config at `step_usd`, no quiet hours.
    fn delta_config(step_usd: f64) -> AlertConfig {
        AlertConfig {
            delta: DeltaConfig {
                enabled: true,
                step_usd,
                quiet: None,
            },
            burst: BurstConfig {
                enabled: false,
                ..BurstConfig::default()
            },
            api_billing: false,
        }
    }

    /// Burst-only config; delta disabled.
    fn burst_config(threshold_usd: f64, window_minutes: u32, cooldown_minutes: u32) -> AlertConfig {
        AlertConfig {
            delta: DeltaConfig {
                enabled: false,
                ..DeltaConfig::default()
            },
            burst: BurstConfig {
                enabled: true,
                threshold_usd,
                window_minutes,
                cooldown_minutes,
                quiet: None,
            },
            api_billing: false,
        }
    }

    /// Runtime whose delta baseline already keys on the evaluation month, so the
    /// first `evaluate` exercises step logic rather than the rollover re-baseline.
    fn runtime_for(now: chrono::DateTime<chrono::Local>, last_step: i64) -> AlertRuntime {
        AlertRuntime {
            delta: DeltaRuntime {
                month_key: month_key(now),
                last_step,
            },
            burst: BurstRuntime::default(),
            permission_lost: false,
        }
    }

    fn sums(burst: f64, mtd: f64) -> Sums {
        Sums {
            burst_window_priced_sum: burst,
            post_launch_priced_mtd: mtd,
        }
    }

    // ---- month_key & quiet-hours primitives ----

    #[test]
    fn month_key_is_year_dash_month() {
        assert_eq!(month_key(local_at(2026, 6, 15, 12, 0)), "2026-06");
        assert_eq!(month_key(local_at(2026, 12, 1, 0, 0)), "2026-12");
        assert_eq!(month_key(local_at(2027, 1, 31, 23, 59)), "2027-01");
    }

    /// I1: a wrap-around quiet window classifies the overnight hours as quiet and
    /// daytime as not-quiet.
    #[test]
    fn quiet_hours_wrap_window_classifies_overnight() {
        let quiet = QuietWindow {
            start: "22:00".into(),
            end: "07:00".into(),
        };
        assert!(
            in_quiet_hours(local_at(2026, 6, 15, 23, 30), &quiet),
            "23:30 is quiet"
        );
        assert!(
            in_quiet_hours(local_at(2026, 6, 16, 2, 0), &quiet),
            "02:00 is quiet"
        );
        assert!(
            !in_quiet_hours(local_at(2026, 6, 15, 8, 0), &quiet),
            "08:00 is awake"
        );
        // Boundaries: start inclusive, end exclusive.
        assert!(
            in_quiet_hours(local_at(2026, 6, 15, 22, 0), &quiet),
            "22:00 start inclusive"
        );
        assert!(
            !in_quiet_hours(local_at(2026, 6, 15, 7, 0), &quiet),
            "07:00 end exclusive"
        );
    }

    #[test]
    fn quiet_hours_same_day_window_is_half_open() {
        let quiet = QuietWindow {
            start: "09:00".into(),
            end: "17:00".into(),
        };
        assert!(in_quiet_hours(local_at(2026, 6, 15, 9, 0), &quiet));
        assert!(in_quiet_hours(local_at(2026, 6, 15, 12, 0), &quiet));
        assert!(
            !in_quiet_hours(local_at(2026, 6, 15, 17, 0), &quiet),
            "end exclusive"
        );
        assert!(!in_quiet_hours(local_at(2026, 6, 15, 8, 59), &quiet));
    }

    #[test]
    fn quiet_hours_equal_start_end_is_unset() {
        let quiet = QuietWindow {
            start: "00:00".into(),
            end: "00:00".into(),
        };
        assert!(!in_quiet_hours(local_at(2026, 6, 15, 0, 0), &quiet));
        assert!(!in_quiet_hours(local_at(2026, 6, 15, 13, 0), &quiet));
    }

    #[test]
    fn quiet_hours_malformed_window_is_not_quiet() {
        let quiet = QuietWindow {
            start: "nonsense".into(),
            end: "07:00".into(),
        };
        assert!(
            !in_quiet_hours(at_noon(), &quiet),
            "unparseable window never suppresses"
        );
    }

    // ---- delta ----

    #[test]
    fn delta_fires_once_per_crossed_step_then_silent_at_same_step() {
        let config = delta_config(50.0);
        // Crossing $50 from a baseline of step 0 fires exactly one delta.
        let runtime = runtime_for(at_noon(), 0);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(0.0, 55.0));
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].rule_type, RuleType::Delta);
        assert_eq!(after.delta.last_step, 1, "baseline advanced to step 1");

        // Immediate re-eval at the same MTD: no new step, silent.
        let (notes2, after2) = evaluate(at_noon(), &config, &after, sums(0.0, 55.0));
        assert!(notes2.is_empty(), "same step must not re-fire");
        assert_eq!(after2.delta.last_step, 1);
    }

    #[test]
    fn delta_below_first_step_does_not_fire() {
        let config = delta_config(50.0);
        let runtime = runtime_for(at_noon(), 0);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(0.0, 49.99));
        assert!(notes.is_empty());
        assert_eq!(after.delta.last_step, 0);
    }

    /// C6: `last_step` pre-bumped (Unit 5's backfill re-baseline) — only post-bump
    /// live growth fires, no retroactive flood of the already-passed steps.
    #[test]
    fn delta_honors_prebumped_baseline_no_retroactive_flood() {
        let config = delta_config(50.0);
        // Backfill silently re-baselined to step 4 ($200 MTD). MTD now reads $230;
        // still step 4 — nothing fires despite four "passed" milestones.
        let runtime = runtime_for(at_noon(), 4);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(0.0, 230.0));
        assert!(notes.is_empty(), "passed milestones must not flood");
        assert_eq!(after.delta.last_step, 4);

        // Now live growth crosses $250 (step 5): exactly one fire.
        let (notes2, after2) = evaluate(at_noon(), &config, &after, sums(0.0, 250.0));
        assert_eq!(notes2.len(), 1, "only the newly-crossed step fires");
        assert_eq!(after2.delta.last_step, 5);
    }

    /// C7: shrinking the step size re-baselines via the same step math — the
    /// engine never replays steps the old size already passed (the caller persists
    /// the advanced `last_step`; a smaller step recomputes the current step and
    /// fires at most once for the new ladder position, not once per passed step).
    #[test]
    fn delta_step_size_edit_does_not_flood_passed_steps() {
        // $100 MTD already at step 1 of a $100 ladder.
        let runtime = runtime_for(at_noon(), 1);
        // User shrinks the step to $25: $100 MTD now sits at step 4. A single
        // evaluation advances to step 4 with one notification, not four.
        let config = delta_config(25.0);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(0.0, 100.0));
        assert_eq!(
            notes.len(),
            1,
            "edit fires at most one milestone, not a flood"
        );
        assert_eq!(after.delta.last_step, 4);
        // Re-eval at the same MTD is silent.
        let (notes2, _) = evaluate(at_noon(), &config, &after, sums(0.0, 100.0));
        assert!(notes2.is_empty());
    }

    #[test]
    fn delta_month_rollover_rebaselines_without_firing() {
        let config = delta_config(50.0);
        // Last month ended at step 6; the stored key is the prior month.
        let runtime = AlertRuntime {
            delta: DeltaRuntime {
                month_key: "2026-05".into(),
                last_step: 6,
            },
            ..AlertRuntime::default()
        };
        // New month's MTD already sits at $120 (step 2) from pre-launch carryover;
        // rollover re-baselines silently to step 2 — no replay of the new month's
        // first two milestones.
        let now = local_at(2026, 6, 15, 12, 0);
        let (notes, after) = evaluate(now, &config, &runtime, sums(0.0, 120.0));
        assert!(notes.is_empty(), "rollover never fires");
        assert_eq!(after.delta.month_key, "2026-06");
        assert_eq!(after.delta.last_step, 2, "re-baselined to current MTD step");

        // Subsequent live growth into step 3 fires once.
        let (notes2, after2) = evaluate(now, &config, &after, sums(0.0, 150.0));
        assert_eq!(notes2.len(), 1);
        assert_eq!(after2.delta.last_step, 3);
    }

    #[test]
    fn delta_disabled_never_fires_but_still_tracks_rollover() {
        let mut config = delta_config(50.0);
        config.delta.enabled = false;
        // Same month, growth crosses a step: disabled means no fire and no advance.
        let runtime = runtime_for(at_noon(), 0);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(0.0, 200.0));
        assert!(notes.is_empty());
        assert_eq!(
            after.delta.last_step, 0,
            "disabled rule does not advance mid-month"
        );

        // Month rollover with a disabled rule: the rollover re-baseline branch
        // fires unconditionally (it is not gated on `enabled`) so the new
        // month's baseline is set to the current MTD step — no notification.
        // Evaluation instant is June; prior runtime is keyed to May.
        let prior_month_runtime = AlertRuntime {
            delta: DeltaRuntime {
                month_key: "2026-05".into(), // a prior month
                last_step: 10,
            },
            ..AlertRuntime::default()
        };
        let now_june = local_at(2026, 6, 15, 12, 0);
        // MTD in the new month is $150 → step 3 at $50/step.
        let (notes2, after2) = evaluate(now_june, &config, &prior_month_runtime, sums(0.0, 150.0));
        assert!(
            notes2.is_empty(),
            "rollover with disabled delta never fires"
        );
        assert_eq!(
            after2.delta.month_key, "2026-06",
            "rollover updates the month key even when delta is disabled"
        );
        assert_eq!(
            after2.delta.last_step, 3,
            "rollover re-baselines last_step to current MTD step"
        );
    }

    /// R17: a delta milestone that would fire but lands in quiet hours is dropped
    /// silently — no notification — yet the baseline still advances so it never
    /// re-fires once quiet hours end (no pending flag).
    #[test]
    fn delta_in_quiet_hours_drops_silently_but_advances_state() {
        let mut config = delta_config(50.0);
        config.delta.quiet = Some(QuietWindow {
            start: "22:00".into(),
            end: "07:00".into(),
        });
        // 23:30 is inside quiet hours; MTD crosses $100 (step 2 from step 1).
        let now = local_at(2026, 6, 15, 23, 30);
        let runtime = runtime_for(now, 1);
        let (notes, after) = evaluate(now, &config, &runtime, sums(0.0, 100.0));
        assert!(notes.is_empty(), "quiet hours suppress the notification");
        assert_eq!(
            after.delta.last_step, 2,
            "state advances so it never re-fires later"
        );

        // After quiet hours (08:00) at the same MTD: nothing pending, stays silent.
        let later = local_at(2026, 6, 16, 8, 0);
        let (notes2, _) = evaluate(later, &config, &after, sums(0.0, 100.0));
        assert!(notes2.is_empty(), "suppressed step is gone, not deferred");
    }

    // ---- burst ----

    /// R14/R15: a rolling sum at/over threshold fires and arms the cooldown; an
    /// over-threshold eval inside the cooldown does not fire; after the cooldown
    /// elapses it fires again.
    #[test]
    fn burst_fires_arms_cooldown_then_refires_after_it_elapses() {
        let config = burst_config(10.0, 10, 15);
        let t0 = at_noon();
        let runtime = AlertRuntime::default();
        let (notes, after) = evaluate(t0, &config, &runtime, sums(12.0, 0.0));
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].rule_type, RuleType::Burst);
        let armed_until = t0.timestamp_millis() + 15 * 60_000;
        assert_eq!(after.burst.cooldown_until_ms, armed_until);

        // 5 minutes later, still over threshold, but inside the cooldown: silent.
        let t1 = local_at(2026, 6, 15, 12, 5);
        let (notes2, after2) = evaluate(t1, &config, &after, sums(20.0, 0.0));
        assert!(notes2.is_empty(), "cooldown suppresses a second fire");
        assert_eq!(
            after2.burst.cooldown_until_ms, armed_until,
            "cooldown unchanged"
        );

        // 16 minutes after the first fire: cooldown elapsed, fires again.
        let t2 = local_at(2026, 6, 15, 12, 16);
        let (notes3, after3) = evaluate(t2, &config, &after2, sums(11.0, 0.0));
        assert_eq!(notes3.len(), 1, "fires again once cooldown elapses");
        assert_eq!(
            after3.burst.cooldown_until_ms,
            t2.timestamp_millis() + 15 * 60_000
        );
    }

    /// Storm guard: the query floored pre-launch spend out, so the window sum the
    /// engine sees is below threshold — no fire. (The floor itself is a Unit 2
    /// concern; here we assert the engine doesn't fire on a sub-threshold sum.)
    #[test]
    fn burst_below_threshold_does_not_fire() {
        let config = burst_config(10.0, 10, 15);
        // Only pre-launch spend existed; the floored query returns $0.50.
        let (notes, after) = evaluate(at_noon(), &config, &AlertRuntime::default(), sums(0.5, 0.0));
        assert!(notes.is_empty(), "sub-threshold floored sum must not fire");
        assert_eq!(
            after.burst.cooldown_until_ms, 0,
            "no fire leaves cooldown unarmed"
        );
    }

    #[test]
    fn burst_exactly_at_threshold_fires() {
        let config = burst_config(10.0, 10, 15);
        let (notes, _) = evaluate(
            at_noon(),
            &config,
            &AlertRuntime::default(),
            sums(10.0, 0.0),
        );
        assert_eq!(notes.len(), 1, "threshold is inclusive (>=)");
    }

    #[test]
    fn burst_disabled_never_fires() {
        let mut config = burst_config(10.0, 10, 15);
        config.burst.enabled = false;
        let (notes, _) = evaluate(
            at_noon(),
            &config,
            &AlertRuntime::default(),
            sums(99.0, 0.0),
        );
        assert!(notes.is_empty());
    }

    /// R17 (burst): an over-threshold crossing inside quiet hours fires nothing
    /// and leaves the cooldown unarmed, so the next non-quiet crossing is free.
    #[test]
    fn burst_in_quiet_hours_drops_silently_and_leaves_cooldown_unarmed() {
        let mut config = burst_config(10.0, 10, 15);
        config.burst.quiet = Some(QuietWindow {
            start: "22:00".into(),
            end: "07:00".into(),
        });
        let quiet_now = local_at(2026, 6, 15, 23, 0);
        let (notes, after) = evaluate(
            quiet_now,
            &config,
            &AlertRuntime::default(),
            sums(50.0, 0.0),
        );
        assert!(
            notes.is_empty(),
            "quiet hours suppress the burst notification"
        );
        assert_eq!(
            after.burst.cooldown_until_ms, 0,
            "no fire, no cooldown armed"
        );

        // After quiet hours, a fresh over-threshold crossing fires (no pending).
        let awake = local_at(2026, 6, 16, 8, 0);
        let (notes2, _) = evaluate(awake, &config, &after, sums(50.0, 0.0));
        assert_eq!(notes2.len(), 1);
    }

    /// M2: the cooldown is compared in UTC ms, so a DST fall-back that repeats a
    /// local hour cannot reopen the cooldown early. We arm a cooldown and then
    /// evaluate at a UTC instant that is *inside* the cooldown even though the
    /// local wall clock would read an hour earlier on a repeated hour.
    #[test]
    fn burst_cooldown_is_utc_ms_unaffected_by_dst_fallback() {
        let config = burst_config(10.0, 10, 15);
        // Arm a cooldown ending 15 minutes from a reference UTC instant.
        let armed_until = at_noon().timestamp_millis() + 15 * 60_000;
        let runtime = AlertRuntime {
            burst: BurstRuntime {
                cooldown_until_ms: armed_until,
            },
            ..AlertRuntime::default()
        };
        // An instant whose UTC ms is 5 minutes before the cooldown ends: even if a
        // DST fall-back made the *local* hour repeat, the UTC-ms comparison still
        // sees it inside the cooldown, so it must not fire.
        use chrono::TimeZone;
        let inside = chrono::Local
            .timestamp_millis_opt(armed_until - 5 * 60_000)
            .unwrap();
        let (notes, _) = evaluate(inside, &config, &runtime, sums(99.0, 0.0));
        assert!(
            notes.is_empty(),
            "UTC-ms cooldown ignores any repeated local hour"
        );

        // One ms past the cooldown (UTC) fires.
        let past = chrono::Local.timestamp_millis_opt(armed_until).unwrap();
        let (notes2, _) = evaluate(past, &config, &runtime, sums(99.0, 0.0));
        assert_eq!(notes2.len(), 1, "cooldown end is inclusive of now >= until");
    }

    // ---- combined + copy ----

    #[test]
    fn delta_and_burst_can_both_fire_in_one_evaluation() {
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
                quiet: None,
            },
            api_billing: false,
        };
        let runtime = runtime_for(at_noon(), 0);
        let (notes, after) = evaluate(at_noon(), &config, &runtime, sums(15.0, 60.0));
        let kinds: Vec<RuleType> = notes.iter().map(|n| n.rule_type).collect();
        assert!(kinds.contains(&RuleType::Delta));
        assert!(kinds.contains(&RuleType::Burst));
        assert_eq!(after.delta.last_step, 1);
        assert!(after.burst.cooldown_until_ms > 0);
    }

    #[test]
    fn copy_switches_to_real_money_under_api_billing() {
        let mut config = delta_config(50.0);
        config.api_billing = true;
        let runtime = runtime_for(at_noon(), 0);
        let (notes, _) = evaluate(at_noon(), &config, &runtime, sums(0.0, 50.0));
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].title.contains("Spend") && notes[0].body.contains("spent"),
            "api_billing copy is real-money framed: {:?}",
            notes[0]
        );

        // Neutral framing when api_billing is off.
        config.api_billing = false;
        let (notes2, _) = evaluate(at_noon(), &config, &runtime, sums(0.0, 50.0));
        assert!(notes2[0].title.contains("Usage") && notes2[0].body.contains("usage"));
    }

    #[test]
    fn delta_copy_states_the_crossed_milestone_amount() {
        let config = delta_config(50.0);
        let runtime = runtime_for(at_noon(), 0);
        // $130 MTD crosses step 2 ($100) — copy names $100, the milestone crossed,
        // not the raw MTD.
        let (notes, _) = evaluate(at_noon(), &config, &runtime, sums(0.0, 130.0));
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].body.contains("100"),
            "names the crossed milestone: {:?}",
            notes[0]
        );
    }

    #[test]
    fn notification_serializes_rule_type_to_variant_name() {
        assert_eq!(
            serde_json::to_value(RuleType::Burst).unwrap(),
            serde_json::json!("Burst")
        );
        assert_eq!(
            serde_json::to_value(RuleType::Delta).unwrap(),
            serde_json::json!("Delta")
        );
    }

    // ---- delivery + permission-lost recording (deliver_and_record seam) ----

    /// R5/I6: a `show` that returns `PermissionDenied` (the user revoked
    /// notification permission) flips `permission_lost` on the persisted runtime,
    /// while the dedup state the engine advanced is preserved.
    #[test]
    fn denied_show_sets_permission_lost_and_keeps_dedup_state() {
        use tauri::plugin::PermissionState;
        let notes = vec![Notification {
            rule_type: RuleType::Burst,
            title: "Usage spike".into(),
            body: "x".into(),
        }];
        // Engine armed a cooldown; the delivery is then denied.
        let armed = AlertRuntime {
            burst: BurstRuntime {
                cooldown_until_ms: 123,
            },
            ..AlertRuntime::default()
        };
        let out = deliver_and_record(&notes, armed, PermissionState::Denied, |_| {
            crate::notify::ShowOutcome::PermissionDenied
        });
        assert!(out.permission_lost, "a denied show records permission_lost");
        assert_eq!(
            out.burst.cooldown_until_ms, 123,
            "dedup state is preserved so a recovered permission doesn't replay"
        );
    }

    /// A delivered notification leaves `permission_lost` false; multiple notes
    /// are all delivered.
    #[test]
    fn delivered_shows_do_not_set_permission_lost() {
        use tauri::plugin::PermissionState;
        let notes = vec![
            Notification {
                rule_type: RuleType::Burst,
                title: "a".into(),
                body: "b".into(),
            },
            Notification {
                rule_type: RuleType::Delta,
                title: "c".into(),
                body: "d".into(),
            },
        ];
        let mut shown = 0;
        let out = deliver_and_record(
            &notes,
            AlertRuntime::default(),
            PermissionState::Granted,
            |_| {
                shown += 1;
                crate::notify::ShowOutcome::Delivered
            },
        );
        assert_eq!(shown, 2, "every notification is delivered");
        assert!(!out.permission_lost);
    }

    /// Even a single denied delivery among several flips the flag.
    #[test]
    fn any_denied_delivery_flips_permission_lost() {
        use tauri::plugin::PermissionState;
        let notes = vec![
            Notification {
                rule_type: RuleType::Burst,
                title: "a".into(),
                body: "b".into(),
            },
            Notification {
                rule_type: RuleType::Delta,
                title: "c".into(),
                body: "d".into(),
            },
        ];
        let mut first = true;
        // current_permission is Granted but one show() returns PermissionDenied —
        // belt-and-suspenders path: the individual denial still flips the flag.
        let out = deliver_and_record(
            &notes,
            AlertRuntime::default(),
            PermissionState::Granted,
            |_| {
                if std::mem::take(&mut first) {
                    crate::notify::ShowOutcome::Delivered
                } else {
                    crate::notify::ShowOutcome::PermissionDenied
                }
            },
        );
        assert!(out.permission_lost);
    }

    /// Permission-lost clears when the OS state is Granted on the next cycle,
    /// even with no notifications to deliver (pure permission re-check path).
    /// This is the clearing path that makes permission_lost track the actual
    /// OS state rather than latching permanently once set.
    #[test]
    fn permission_lost_clears_when_os_grants_permission_on_next_cycle() {
        use tauri::plugin::PermissionState;
        // Prior cycle recorded permission_lost = true.
        let prior = AlertRuntime {
            permission_lost: true,
            ..AlertRuntime::default()
        };
        // This cycle: no notifications to send, OS now reports Granted.
        let out = deliver_and_record(&[], prior, PermissionState::Granted, |_| {
            crate::notify::ShowOutcome::Delivered // never called
        });
        assert!(
            !out.permission_lost,
            "Granted OS permission clears permission_lost even with no notifications"
        );

        // Denied OS state sets the flag even with no notifications.
        let cleared = AlertRuntime {
            permission_lost: false,
            ..AlertRuntime::default()
        };
        let out2 = deliver_and_record(&[], cleared, PermissionState::Denied, |_| {
            crate::notify::ShowOutcome::Delivered
        });
        assert!(
            out2.permission_lost,
            "Denied OS permission sets permission_lost even with no notifications to deliver"
        );
    }

    // ---- debounce gate (should_run_ingest_eval) ----

    #[test]
    fn ingest_eval_debounce_coalesces_rapid_calls() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));

        // First call after launch always runs (last-eval is 0).
        assert!(state.should_run_ingest_eval(1_000_000));
        // A call 4.999s later is coalesced out.
        assert!(!state.should_run_ingest_eval(1_000_000 + 4_999));
        // Exactly 5s later runs again (claims the slot).
        assert!(state.should_run_ingest_eval(1_000_000 + 5_000));
        // Immediately after, coalesced again.
        assert!(!state.should_run_ingest_eval(1_000_000 + 5_001));
    }

    #[test]
    fn ingest_eval_debounce_first_call_always_runs() {
        let dir = tempfile::tempdir().unwrap();
        let state = AlertState::load(test_db(&dir));
        // The very first ingest after launch must evaluate (a runaway loop that
        // starts seconds after boot must not be missed by the debounce). Real
        // unix-ms clocks are ~1.7e12, far past the 5s window from the `0`
        // "never run" sentinel, so the first real-clock call always passes.
        let now = chrono::Local::now().timestamp_millis();
        assert!(state.should_run_ingest_eval(now));
    }

    // ---- gather_and_apply over a mock-runtime app ----
    //
    // The mock runtime has no notification backend, so the assertion target is the
    // *persisted runtime* the orchestrator writes (the evaluate+show()-seam
    // contract): an armed burst cooldown means burst fired; an advanced delta
    // last_step means delta fired. The notification plugin (desktop) reports
    // `Granted` unconditionally, so `show` "delivers" and `permission_lost` stays
    // false. Real OS delivery is manual-bundle-only (see the plan).

    /// Build a mock app managing a fresh `DbState` + `AlertState` (sharing one DB
    /// handle) with the notification plugin registered so `show` reports
    /// `Granted`. Returns the app and the shared DB handle for row inserts.
    fn mock_app_with_alerts() -> (
        tauri::App<tauri::test::MockRuntime>,
        Arc<Mutex<Db>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()));
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_notification::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DbState(Arc::clone(&db)));
        app.manage(AlertState::load(Arc::clone(&db)));
        (app, db, dir)
    }

    /// Insert a priced `api_request` row at `timestamp_ms`.
    fn insert_priced(db: &Mutex<Db>, timestamp_ms: i64, cost_usd: f64, source: &str) {
        let db = db.lock().unwrap();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type, source)
                 VALUES ('sess', ?1, ?2, 'api_request', ?3)",
                rusqlite::params![timestamp_ms, cost_usd, source],
            )
            .unwrap();
    }

    /// Storm guard: rows recovered from *before* launch — one tagged `backfill`,
    /// one flipped to `otel` (the re-delivered-export case) — both keep their real
    /// pre-launch `timestamp_ms`, so the `process_start_ms` floor excludes them
    /// from both the burst window and the delta MTD. No alert fires.
    ///
    /// THIS TEST MUST INCLUDE THE OTEL-FLIP ROW (per the plan): a `source='otel'`
    /// filter would be defeated by it, but the event-time floor is not.
    #[test]
    fn storm_guard_excludes_prelaunch_rows_even_after_otel_flip() {
        let (app, db, _dir) = mock_app_with_alerts();
        let state = app.state::<AlertState>();
        let launch = state.process_start_ms();

        // Two big pre-launch rows inside the 10-min burst window by wall-clock,
        // but before launch by event time. One is plain backfill; the other was
        // flipped to otel with its real (pre-launch) timestamp by a re-delivered
        // export — the exact case `source` filtering can't catch.
        insert_priced(&db, launch - 60_000, 500.0, "backfill");
        insert_priced(&db, launch - 30_000, 500.0, "otel");

        // Enable both rules so either firing would be observable.
        let mut config = AlertConfig::default();
        config.delta.enabled = true;
        config.delta.step_usd = 50.0;
        config.burst.threshold_usd = 10.0;
        state.set_config(config).unwrap();

        gather_and_apply(app.handle());

        let runtime = app.state::<AlertState>().runtime();
        assert_eq!(
            runtime.burst.cooldown_until_ms, 0,
            "pre-launch spend must not arm the burst cooldown"
        );
        // Delta: the rollover/first-eval re-baseline keys the month and sets
        // last_step to the (floored, post-launch) MTD step, which is 0 here.
        assert_eq!(
            runtime.delta.last_step, 0,
            "pre-launch spend must not advance the delta ladder"
        );
        assert!(!runtime.permission_lost);
    }

    /// I4: two over-threshold otel batches 30s apart produce exactly one burst —
    /// the cooldown armed by the first eval persists and suppresses the second.
    #[test]
    fn two_overthreshold_batches_fire_exactly_one_burst() {
        let (app, db, _dir) = mock_app_with_alerts();
        let state = app.state::<AlertState>();
        let launch = state.process_start_ms();

        // Burst only (default delta disabled). First batch: $20 of post-launch
        // otel spend stamped at `launch` (== the process_start floor). The short
        // sleep guarantees the eval's `now` is strictly after `launch`, so the
        // row falls inside `[floor, now)` deterministically (a row stamped at the
        // exact `now` ms would be excluded by the window's exclusive end).
        insert_priced(&db, launch, 20.0, "otel");
        std::thread::sleep(std::time::Duration::from_millis(5));
        gather_and_apply(app.handle());
        let after_first = app.state::<AlertState>().runtime();
        assert!(
            after_first.burst.cooldown_until_ms > 0,
            "first over-threshold batch arms the cooldown (burst fired)"
        );

        // Second batch still over threshold, but inside the 15-min cooldown: the
        // persisted cooldown suppresses a second fire.
        insert_priced(&db, launch + 1, 20.0, "otel");
        gather_and_apply(app.handle());
        let after_second = app.state::<AlertState>().runtime();
        assert_eq!(
            after_second.burst.cooldown_until_ms, after_first.burst.cooldown_until_ms,
            "the second batch must not re-arm the cooldown (no double-fire)"
        );
    }

    /// Concurrency: a config-save eval (via the public seam) and an ingest eval
    /// racing on the same state serialize through the eval lock, so the burst
    /// cooldown is armed exactly once — no lost update, no double-fire.
    #[test]
    fn racing_config_and_ingest_evals_serialize_without_double_fire() {
        let (app, db, _dir) = mock_app_with_alerts();
        let state = app.state::<AlertState>();
        let launch = state.process_start_ms();
        insert_priced(&db, launch, 50.0, "otel");
        std::thread::sleep(std::time::Duration::from_millis(5)); // now > launch

        // Spawn two threads that both drive a full evaluation cycle against the
        // shared managed state. The eval lock must serialize them.
        let h1 = app.handle().clone();
        let h2 = app.handle().clone();
        let t1 = std::thread::spawn(move || gather_and_apply(&h1));
        let t2 = std::thread::spawn(move || gather_and_apply(&h2));
        t1.join().unwrap();
        t2.join().unwrap();

        let runtime = app.state::<AlertState>().runtime();
        assert!(
            runtime.burst.cooldown_until_ms > 0,
            "the burst fired and armed a cooldown"
        );
        // Whichever eval ran first armed the cooldown; the second saw it armed
        // and did not re-fire. We can't pin the exact ms (wall clock), but the
        // lock guarantees a single coherent runtime, never an interleaved one.
        assert!(!runtime.permission_lost);
    }

    /// A config save runs a re-evaluation through the same orchestrator: enabling
    /// the burst rule with in-window spend fires immediately on save. The eval is
    /// now offloaded to spawn_blocking so we give the thread pool a short window
    /// to complete (the task is submitted synchronously before the command returns,
    /// so it runs promptly — this is not a race, just async scheduling).
    #[test]
    fn config_save_reevaluates_immediately() {
        let (app, db, _dir) = mock_app_with_alerts();
        let launch = app.state::<AlertState>().process_start_ms();
        insert_priced(&db, launch, 30.0, "otel");
        std::thread::sleep(std::time::Duration::from_millis(5)); // now > launch

        // Save a config with burst enabled at a $10 threshold; the save-time
        // re-eval should arm the cooldown right away.
        let config = AlertConfig {
            burst: BurstConfig {
                enabled: true,
                threshold_usd: 10.0,
                ..BurstConfig::default()
            },
            ..AlertConfig::default()
        };
        alert_config_set(app.handle().clone(), config).expect("set");

        // The re-eval was submitted to the thread pool synchronously before the
        // command returned. Wait up to 500ms for it to complete (generous; the
        // actual task completes in single-digit ms).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let runtime = app.state::<AlertState>().runtime();
            if runtime.burst.cooldown_until_ms > 0 {
                break; // eval completed and fired
            }
            if std::time::Instant::now() >= deadline {
                panic!("a config save re-evaluates and can fire immediately (timed out waiting)");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// `gather_and_apply` tolerates an app with no managed `AlertState`/`DbState`
    /// (startup ordering / minimal test apps): a no-op, never a panic.
    #[test]
    fn gather_and_apply_tolerates_missing_state() {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_notification::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        gather_and_apply(app.handle()); // nothing managed yet
    }

    /// The backfill re-baseline bumps `last_step` to the current post-launch MTD
    /// step silently (no fire) and keys the current month, so a later live eval
    /// only fires on growth past it.
    #[test]
    fn rebaseline_delta_bumps_last_step_to_current_mtd_silently() {
        let (app, db, _dir) = mock_app_with_alerts();
        let state = app.state::<AlertState>();
        let launch = state.process_start_ms();

        // Enable delta; $230 of post-launch month spend at a $50 step → step 4.
        let mut config = AlertConfig::default();
        config.delta.enabled = true;
        config.delta.step_usd = 50.0;
        state.set_config(config).unwrap();
        insert_priced(&db, launch, 230.0, "otel");

        rebaseline_delta_now(app.handle());

        let runtime = app.state::<AlertState>().runtime();
        assert_eq!(runtime.delta.last_step, 4, "re-baselined to floor(230/50)");
        assert_eq!(runtime.delta.month_key, month_key(chrono::Local::now()));

        // A subsequent live eval at the same MTD fires nothing (already baselined).
        gather_and_apply(app.handle());
        let after = app.state::<AlertState>().runtime();
        assert_eq!(
            after.delta.last_step, 4,
            "no retroactive flood after re-baseline"
        );
    }
}
