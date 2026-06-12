//! Today-metrics queries for the menu bar popover (tasks 4.2/4.3).
//!
//! One read-only command ([`today_metrics`]) that aggregates everything the
//! popover renders for the current local calendar day:
//!
//! - total cost (API-equivalent; rows with unknown pricing are surfaced via
//!   `unpriced_requests` instead of silently counting as $0)
//! - the four token counts (input / output / cache read / cache creation)
//! - distinct sessions active today: `COUNT(DISTINCT session_id)`, so a
//!   resumed session (same id, new process) never double-counts (PRD FR-7
//!   metric definitions)
//! - top 3 projects by today's cost, mapped through `sessions.cwd`
//!
//! Day boundaries are **local midnight** per the PRD: the window is
//! `[today 00:00 local, tomorrow 00:00 local)`, computed DST-correct via
//! `chrono::Local` (a 23h/25h day on a DST transition stays a single day).
//!
//! Both queries are index-only range scans over `idx_requests_facet_rollup`
//! (schema v4; its key starts with the v3 `idx_requests_time_rollup`
//! columns it replaced), so they touch only today's index pages regardless
//! of table size (<100ms popover budget, NFR).
//!
//! A second command ([`daily_costs`], task 4.3) returns the per-day cost
//! series behind the popover sparkline: one bucket per local calendar day
//! for the trailing N days (today inclusive), each bucket aggregated with
//! the same `[local midnight, next local midnight)` window as
//! [`today_metrics`] so the last sparkline bar always equals today's
//! headline cost. Days with no rows come back as explicit zero buckets;
//! the frontend never has to infer gaps.

use serde::Serialize;
use tauri::{Manager, Runtime};

use crate::db::{Db, DbState};

/// How many projects the popover lists.
const TOP_PROJECTS_LIMIT: u32 = 3;

/// One project rollup in the top-projects list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectCost {
    /// Session working directory. `None` groups requests whose session has
    /// no known cwd (hook missed and backfill could not heal it yet).
    pub cwd: Option<String>,
    /// API-equivalent cost of this project's requests today.
    pub cost_usd: f64,
    /// `api_request` rows behind that cost.
    pub requests: i64,
}

/// Everything the popover renders, in one query pass.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TodayMetrics {
    /// Local midnight opening the window (unix ms, inclusive).
    pub day_start_ms: i64,
    /// Next local midnight closing the window (unix ms, exclusive).
    pub day_end_ms: i64,
    /// Total API-equivalent cost; unpriced rows contribute nothing.
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// `api_request` rows today (errors excluded).
    pub requests: i64,
    /// `api_request` rows with no `cost_usd` (unknown model pricing): their
    /// tokens are counted above but the cost total excludes them.
    pub unpriced_requests: i64,
    /// Distinct `session_id`s active today; resumes don't double-count.
    pub sessions: i64,
    /// Top projects by cost, descending, at most [`TOP_PROJECTS_LIMIT`].
    pub top_projects: Vec<ProjectCost>,
}

/// Unix ms of local midnight opening `date`. DST-correct: when a transition
/// makes 00:00 ambiguous the earlier instant wins, and when it skips 00:00
/// entirely (some zones spring forward at midnight) the first existing
/// local time that day is the boundary.
pub(crate) fn local_midnight_ms(date: chrono::NaiveDate) -> i64 {
    use chrono::{Duration, LocalResult, TimeZone};
    let naive = date.and_hms_opt(0, 0, 0).expect("00:00:00 is always valid");
    for half_hours in 0..6 {
        match chrono::Local.from_local_datetime(&(naive + Duration::minutes(30 * half_hours))) {
            LocalResult::Single(dt) => return dt.timestamp_millis(),
            LocalResult::Ambiguous(earliest, _) => return earliest.timestamp_millis(),
            LocalResult::None => continue,
        }
    }
    // No real timezone skips more than 3h at once; interpret as UTC rather
    // than panic if one ever does.
    naive.and_utc().timestamp_millis()
}

/// The current local day as a `[start, end)` unix-ms window.
pub fn local_day_window(now: chrono::DateTime<chrono::Local>) -> (i64, i64) {
    let today = now.date_naive();
    let tomorrow = today.succ_opt().expect("not at the end of the calendar");
    (local_midnight_ms(today), local_midnight_ms(tomorrow))
}

/// Unix ms of local midnight on the first day of `date`'s month. The start of
/// the calendar-month window; mirrors [`local_midnight_ms`] so it is DST-correct
/// by construction (the month boundary lands on local midnight regardless of a
/// spring-forward/fall-back inside the month).
pub(crate) fn local_month_start_ms(date: chrono::NaiveDate) -> i64 {
    use chrono::Datelike;
    let first = date.with_day(1).expect("day 1 is valid for every month");
    local_midnight_ms(first)
}

/// The current local calendar month as a `[start, end)` unix-ms window: the
/// first of this month at 00:00 local up to (exclusive) the first of next month
/// at 00:00 local. The shared month-boundary primitive (Budgets reuses it).
/// Mirrors [`local_day_window`]; DST-correct because both ends route through
/// [`local_midnight_ms`].
pub fn local_month_window(now: chrono::DateTime<chrono::Local>) -> (i64, i64) {
    use chrono::Datelike;
    let today = now.date_naive();
    // First of next month: advancing one calendar month, then clamping to the
    // first, sidesteps month-length and year-rollover arithmetic (Dec -> Jan).
    let next_month = if today.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
    }
    .expect("first of next month is always a valid date");
    (local_month_start_ms(today), local_midnight_ms(next_month))
}

/// Aggregate the metrics for one `[day_start_ms, day_end_ms)` window.
/// Pure DB read; the window is a parameter so tests pin it exactly.
pub fn metrics_for_window(
    db: &Db,
    day_start_ms: i64,
    day_end_ms: i64,
) -> Result<TodayMetrics, rusqlite::Error> {
    let conn = db.conn();

    // Totals + distinct sessions in one scan of today's rows. `api_error`
    // rows carry zero tokens and NULL cost, so they cannot skew the sums,
    // but their session still counts as active (the user was working).
    let mut metrics = conn.query_row(
        "SELECT
            COALESCE(SUM(cost_usd), 0.0),
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(cache_read_tokens), 0),
            COALESCE(SUM(cache_creation_tokens), 0),
            COALESCE(SUM(event_type = 'api_request'), 0),
            COALESCE(SUM(event_type = 'api_request' AND cost_usd IS NULL), 0),
            COUNT(DISTINCT session_id)
         FROM requests
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
        (day_start_ms, day_end_ms),
        |row| {
            Ok(TodayMetrics {
                day_start_ms,
                day_end_ms,
                cost_usd: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                cache_read_tokens: row.get(3)?,
                cache_creation_tokens: row.get(4)?,
                requests: row.get(5)?,
                unpriced_requests: row.get(6)?,
                sessions: row.get(7)?,
                top_projects: Vec::new(),
            })
        },
    )?;

    // Top projects by cost. Sessions with no row or a NULL cwd group
    // together as the "unknown project" bucket (cwd = NULL).
    let mut stmt = conn.prepare(
        "SELECT s.cwd, COALESCE(SUM(r.cost_usd), 0.0) AS cost, COUNT(*)
         FROM requests r
         LEFT JOIN sessions s ON s.session_id = r.session_id
         WHERE r.timestamp_ms >= ?1 AND r.timestamp_ms < ?2
           AND r.event_type = 'api_request'
         GROUP BY s.cwd
         ORDER BY cost DESC
         LIMIT ?3",
    )?;
    metrics.top_projects = stmt
        .query_map((day_start_ms, day_end_ms, TOP_PROJECTS_LIMIT), |row| {
            Ok(ProjectCost {
                cwd: row.get(0)?,
                cost_usd: row.get(1)?,
                requests: row.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    Ok(metrics)
}

/// Priced-only spend over a `[start, end)` window for the alert engine.
///
/// Returns `(priced_sum, unpriced_count)`:
/// - `priced_sum` totals `cost_usd` over rows that actually carry a price, so
///   `NULL`-cost rows never silently count as `$0` (the COALESCE-to-zero trap
///   `docs/notes/pricing.md` warns about): the alert thresholds must reflect
///   real priced spend, not an under-count padded with free zeros.
/// - `unpriced_count` is the `api_request` rows *excluded* from that sum
///   because their `cost_usd` is `NULL` (unknown model pricing) inside the
///   window, so a caller can tell "$0 of spend" from "spend it can't see".
///
/// `min_timestamp_ms` is an optional event-time floor: when `Some(floor)`, only
/// rows timestamped at/after `floor` are counted. Burst passes
/// `max(now - window, process_start_ms)` so spend recovered from before this
/// process launched (backfill, or an otel re-delivery that flips a backfill row
/// and resets its `timestamp_ms` to the real event time) can never trip a
/// rate alert; `None` applies no floor.
///
/// One indexed range scan, mirroring [`metrics_for_window`]: the conditional
/// sums ride a single pass of `idx_requests_facet_rollup` (timestamp_ms-leading,
/// pinned so the planner can't drift onto a non-time index).
pub fn priced_spend_for_window(
    db: &Db,
    start_ms: i64,
    end_ms: i64,
    min_timestamp_ms: Option<i64>,
) -> Result<(f64, i64), rusqlite::Error> {
    let conn = db.conn();
    // A floor of `None` collapses to a tautology (`timestamp_ms >= MIN`) so the
    // SQL shape is fixed regardless and the planner sees one parameterized
    // statement. The window's own `>= ?1` already keeps the scan bounded.
    let floor = min_timestamp_ms.unwrap_or(i64::MIN);
    conn.query_row(
        "SELECT
            COALESCE(SUM(cost_usd), 0.0),
            COALESCE(SUM(event_type = 'api_request' AND cost_usd IS NULL), 0)
         FROM requests INDEXED BY idx_requests_facet_rollup
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2 AND timestamp_ms >= ?3",
        (start_ms, end_ms, floor),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

/// Frontend query: today's metrics for the popover.
#[tauri::command]
pub fn today_metrics<R: Runtime>(app: tauri::AppHandle<R>) -> Result<TodayMetrics, String> {
    let (day_start_ms, day_end_ms) = local_day_window(chrono::Local::now());
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    metrics_for_window(&db, day_start_ms, day_end_ms)
        .map_err(|err| format!("cannot query today's metrics: {err}"))
}

/// Longest series [`daily_costs`] will return (a year of daily bars).
const MAX_SPARKLINE_DAYS: u32 = 366;

/// One day's cost bucket in the sparkline series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailyCost {
    /// Local midnight opening this day (unix ms, inclusive).
    pub day_start_ms: i64,
    /// API-equivalent cost; unpriced rows contribute nothing (same rule as
    /// the headline total).
    pub cost_usd: f64,
    /// `api_request` rows that day (errors excluded).
    pub requests: i64,
}

/// Local-midnight boundaries for the trailing `days` calendar days ending
/// today: `days + 1` ascending instants where consecutive pairs bracket one
/// day (today's window is the last pair). Each midnight is resolved
/// independently via [`local_midnight_ms`], so DST transitions inside the
/// range keep every day exactly `[00:00, next 00:00)` local.
pub fn trailing_day_boundaries(days: u32, now: chrono::DateTime<chrono::Local>) -> Vec<i64> {
    let today = now.date_naive();
    let mut boundaries = Vec::with_capacity(days as usize + 1);
    let mut date = today - chrono::Duration::days(i64::from(days) - 1);
    for _ in 0..days {
        boundaries.push(local_midnight_ms(date));
        date = date.succ_opt().expect("not at the end of the calendar");
    }
    boundaries.push(local_midnight_ms(date)); // tomorrow: closes today's window
    boundaries
}

/// Aggregate one cost bucket per `[boundaries[i], boundaries[i+1])` window.
/// Days without rows yield explicit `cost_usd == 0.0, requests == 0`
/// buckets, so the result always has `boundaries.len() - 1` entries in
/// chronological order. Each window is the same indexed range scan
/// `today_metrics` uses, so the buckets match its values exactly.
pub fn daily_cost_series(db: &Db, boundaries: &[i64]) -> Result<Vec<DailyCost>, rusqlite::Error> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT
            COALESCE(SUM(cost_usd), 0.0),
            COALESCE(SUM(event_type = 'api_request'), 0)
         FROM requests
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
    )?;
    boundaries
        .windows(2)
        .map(|window| {
            stmt.query_row((window[0], window[1]), |row| {
                Ok(DailyCost {
                    day_start_ms: window[0],
                    cost_usd: row.get(0)?,
                    requests: row.get(1)?,
                })
            })
        })
        .collect()
}

/// Frontend query: per-day cost buckets for the popover sparkline, oldest
/// first, today last. `days` is clamped to `1..=366`.
#[tauri::command]
pub fn daily_costs<R: Runtime>(
    app: tauri::AppHandle<R>,
    days: u32,
) -> Result<Vec<DailyCost>, String> {
    let days = days.clamp(1, MAX_SPARKLINE_DAYS);
    let boundaries = trailing_day_boundaries(days, chrono::Local::now());
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    daily_cost_series(&db, &boundaries).map_err(|err| format!("cannot query daily costs: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::params;
    use tempfile::TempDir;

    const DAY_MS: i64 = 86_400_000;
    /// Window the tests aggregate over (any fixed "local day" works because
    /// `metrics_for_window` takes the boundaries as parameters).
    const START: i64 = 1_781_150_400_000;
    const END: i64 = START + DAY_MS;

    fn test_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (dir, db)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_request(
        db: &Db,
        session_id: Option<&str>,
        timestamp_ms: i64,
        cost_usd: Option<f64>,
        tokens: (i64, i64, i64, i64),
        event_type: &str,
    ) {
        db.conn()
            .execute(
                "INSERT INTO requests (
                    session_id, timestamp_ms, cost_usd, input_tokens,
                    output_tokens, cache_read_tokens, cache_creation_tokens,
                    event_type
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    timestamp_ms,
                    cost_usd,
                    tokens.0,
                    tokens.1,
                    tokens.2,
                    tokens.3,
                    event_type
                ],
            )
            .unwrap();
    }

    fn insert_session(db: &Db, session_id: &str, cwd: Option<&str>) {
        db.conn()
            .execute(
                "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                 VALUES (?1, ?2, ?3)",
                params![session_id, cwd, START],
            )
            .unwrap();
    }

    // ---- window filtering ----

    #[test]
    fn window_is_inclusive_start_exclusive_end() {
        let (_dir, db) = test_db();
        // One row exactly at each boundary and one inside.
        insert_request(
            &db,
            Some("before"),
            START - 1,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("at-start"),
            START,
            Some(2.0),
            (2, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("inside"),
            START + 1000,
            Some(4.0),
            (4, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("at-end"),
            END,
            Some(8.0),
            (8, 0, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.cost_usd, 6.0);
        assert_eq!(metrics.input_tokens, 6);
        assert_eq!(metrics.requests, 2);
        assert_eq!(metrics.sessions, 2);
    }

    #[test]
    fn empty_db_yields_zeros_and_no_projects() {
        let (_dir, db) = test_db();
        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(
            metrics,
            TodayMetrics {
                day_start_ms: START,
                day_end_ms: END,
                cost_usd: 0.0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                requests: 0,
                unpriced_requests: 0,
                sessions: 0,
                top_projects: vec![],
            }
        );
    }

    // ---- sessions ----

    #[test]
    fn resumed_session_counts_once() {
        let (_dir, db) = test_db();
        // Same session id across many requests (a resume keeps the id).
        for i in 0..5 {
            insert_request(
                &db,
                Some("sess-resumed"),
                START + i * 60_000,
                Some(0.1),
                (10, 5, 0, 0),
                "api_request",
            );
        }
        insert_request(
            &db,
            Some("sess-other"),
            START + 1,
            Some(0.1),
            (1, 1, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.sessions, 2);
        assert_eq!(metrics.requests, 6);
    }

    #[test]
    fn null_session_ids_do_not_count_as_a_session() {
        let (_dir, db) = test_db();
        insert_request(&db, None, START + 1, Some(0.5), (1, 1, 0, 0), "api_request");
        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.sessions, 0);
        assert_eq!(metrics.requests, 1);
        assert_eq!(metrics.cost_usd, 0.5);
    }

    #[test]
    fn error_rows_count_the_session_but_not_the_request() {
        let (_dir, db) = test_db();
        insert_request(
            &db,
            Some("sess-err"),
            START + 1,
            None,
            (0, 0, 0, 0),
            "api_error",
        );
        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.sessions, 1);
        assert_eq!(metrics.requests, 0);
        assert_eq!(
            metrics.unpriced_requests, 0,
            "errors are not unpriced requests"
        );
        assert_eq!(metrics.cost_usd, 0.0);
    }

    // ---- token split & pricing ----

    #[test]
    fn token_split_sums_all_four_counters() {
        let (_dir, db) = test_db();
        insert_request(
            &db,
            Some("s"),
            START + 1,
            Some(1.5),
            (100, 20, 3000, 400),
            "api_request",
        );
        insert_request(
            &db,
            Some("s"),
            START + 2,
            Some(0.5),
            (1, 2, 3, 4),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.input_tokens, 101);
        assert_eq!(metrics.output_tokens, 22);
        assert_eq!(metrics.cache_read_tokens, 3003);
        assert_eq!(metrics.cache_creation_tokens, 404);
        assert_eq!(metrics.cost_usd, 2.0);
    }

    #[test]
    fn unpriced_rows_count_tokens_but_not_cost() {
        let (_dir, db) = test_db();
        insert_request(
            &db,
            Some("s"),
            START + 1,
            Some(1.0),
            (10, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("s"),
            START + 2,
            None,
            (90, 0, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.cost_usd, 1.0);
        assert_eq!(metrics.input_tokens, 100);
        assert_eq!(metrics.requests, 2);
        assert_eq!(metrics.unpriced_requests, 1);
    }

    // ---- top projects ----

    #[test]
    fn top_projects_ranked_by_cost_limited_to_three() {
        let (_dir, db) = test_db();
        for (i, cost) in [0.1, 5.0, 2.5, 1.0].iter().enumerate() {
            let session = format!("sess-{i}");
            insert_session(&db, &session, Some(&format!("/proj/p{i}")));
            insert_request(
                &db,
                Some(&session),
                START + 1,
                Some(*cost),
                (1, 1, 0, 0),
                "api_request",
            );
        }

        let metrics = metrics_for_window(&db, START, END).unwrap();
        let order: Vec<(Option<&str>, f64)> = metrics
            .top_projects
            .iter()
            .map(|p| (p.cwd.as_deref(), p.cost_usd))
            .collect();
        assert_eq!(
            order,
            vec![
                (Some("/proj/p1"), 5.0),
                (Some("/proj/p2"), 2.5),
                (Some("/proj/p3"), 1.0),
            ]
        );
    }

    #[test]
    fn projects_aggregate_across_sessions_with_same_cwd() {
        let (_dir, db) = test_db();
        insert_session(&db, "sess-a", Some("/proj/shared"));
        insert_session(&db, "sess-b", Some("/proj/shared"));
        insert_request(
            &db,
            Some("sess-a"),
            START + 1,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("sess-b"),
            START + 2,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.top_projects.len(), 1);
        assert_eq!(metrics.top_projects[0].cwd.as_deref(), Some("/proj/shared"));
        assert_eq!(metrics.top_projects[0].cost_usd, 3.0);
        assert_eq!(metrics.top_projects[0].requests, 2);
    }

    #[test]
    fn unknown_cwd_groups_into_one_null_bucket() {
        let (_dir, db) = test_db();
        // One session row with NULL cwd, one request with no session row at
        // all: both land in the same NULL bucket.
        insert_session(&db, "sess-null-cwd", None);
        insert_request(
            &db,
            Some("sess-null-cwd"),
            START + 1,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("sess-no-row"),
            START + 2,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_session(&db, "sess-known", Some("/proj/known"));
        insert_request(
            &db,
            Some("sess-known"),
            START + 3,
            Some(0.5),
            (1, 0, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.top_projects.len(), 2);
        assert_eq!(metrics.top_projects[0].cwd, None);
        assert_eq!(metrics.top_projects[0].cost_usd, 3.0);
        assert_eq!(metrics.top_projects[0].requests, 2);
        assert_eq!(metrics.top_projects[1].cwd.as_deref(), Some("/proj/known"));
    }

    #[test]
    fn yesterdays_rows_do_not_leak_into_todays_projects() {
        let (_dir, db) = test_db();
        insert_session(&db, "sess-old", Some("/proj/old"));
        insert_request(
            &db,
            Some("sess-old"),
            START - DAY_MS,
            Some(99.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_session(&db, "sess-new", Some("/proj/new"));
        insert_request(
            &db,
            Some("sess-new"),
            START + 1,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );

        let metrics = metrics_for_window(&db, START, END).unwrap();
        assert_eq!(metrics.top_projects.len(), 1);
        assert_eq!(metrics.top_projects[0].cwd.as_deref(), Some("/proj/new"));
        assert_eq!(metrics.cost_usd, 1.0);
    }

    // ---- local day window ----

    #[test]
    fn local_day_window_brackets_now_and_spans_one_day() {
        let now = chrono::Local::now();
        let (start, end) = local_day_window(now);
        let now_ms = now.timestamp_millis();
        assert!(
            start <= now_ms && now_ms < end,
            "now must fall inside its own day"
        );
        // A local day is 24h except on DST transitions (23h/25h).
        assert!(
            (end - start) >= 23 * 3_600_000 && (end - start) <= 25 * 3_600_000,
            "day length out of range: {}ms",
            end - start
        );
        // Midnight boundary: start must render as 00:00 local time.
        use chrono::TimeZone;
        let start_local = chrono::Local.timestamp_millis_opt(start).unwrap();
        assert_eq!(
            start_local.format("%H:%M:%S%.3f").to_string(),
            "00:00:00.000"
        );
    }

    // ---- local month window ----

    #[test]
    fn local_month_window_brackets_now_and_starts_on_the_first_at_midnight() {
        let now = chrono::Local::now();
        let (start, end) = local_month_window(now);
        let now_ms = now.timestamp_millis();
        assert!(
            start <= now_ms && now_ms < end,
            "now must fall inside its own month"
        );
        // A month is 28-31 days, each 24h except DST transitions (23h/25h).
        let len = end - start;
        assert!(
            (27 * DAY_MS..=32 * DAY_MS).contains(&len),
            "month length out of range: {len}ms"
        );
        use chrono::{Datelike, TimeZone};
        let start_local = chrono::Local.timestamp_millis_opt(start).unwrap();
        assert_eq!(start_local.day(), 1, "start is the first of the month");
        assert_eq!(
            start_local.format("%H:%M:%S%.3f").to_string(),
            "00:00:00.000"
        );
        // End is the first of the *next* month at local midnight.
        let end_local = chrono::Local.timestamp_millis_opt(end).unwrap();
        assert_eq!(end_local.day(), 1, "end is the first of next month");
        assert_eq!(end_local.format("%H:%M:%S%.3f").to_string(), "00:00:00.000");
    }

    /// Build the `[start, end)` month window for a fixed local date, exercising
    /// the same boundary math `local_month_window` uses without depending on the
    /// machine's current month.
    fn month_window_for(year: i32, month: u32, day: u32) -> (i64, i64) {
        use chrono::TimeZone;
        let date = chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap();
        let noon = date.and_hms_opt(12, 0, 0).unwrap();
        let now = chrono::Local.from_local_datetime(&noon).single().unwrap();
        local_month_window(now)
    }

    /// Assert a unix-ms instant renders as the first of `month`/`year` at local
    /// midnight.
    fn assert_is_first_at_midnight(ms: i64, year: i32, month: u32) {
        use chrono::{Datelike, TimeZone};
        let local = chrono::Local.timestamp_millis_opt(ms).unwrap();
        assert_eq!(local.year(), year);
        assert_eq!(local.month(), month);
        assert_eq!(local.day(), 1);
        assert_eq!(local.format("%H:%M:%S%.3f").to_string(), "00:00:00.000");
    }

    #[test]
    fn local_month_window_mid_month_spans_first_to_first_of_next() {
        let (start, end) = month_window_for(2026, 6, 15);
        assert_is_first_at_midnight(start, 2026, 6);
        assert_is_first_at_midnight(end, 2026, 7);
    }

    #[test]
    fn local_month_window_handles_december_to_january_rollover() {
        let (start, end) = month_window_for(2026, 12, 20);
        assert_is_first_at_midnight(start, 2026, 12);
        // Year rolls over: next month is January of the following year.
        assert_is_first_at_midnight(end, 2027, 1);
    }

    #[test]
    fn local_month_window_february_leap_and_non_leap() {
        use chrono::TimeZone;
        // 2024 is a leap year: February has 29 days. The window must close on
        // March 1 regardless, and span exactly 29 local days.
        let (leap_start, leap_end) = month_window_for(2024, 2, 10);
        assert_is_first_at_midnight(leap_start, 2024, 2);
        assert_is_first_at_midnight(leap_end, 2024, 3);
        // A non-DST February stays exact 24h days; assert the day count, not raw
        // ms, to stay correct in any timezone.
        let leap_days = (chrono::Local.timestamp_millis_opt(leap_end).unwrap()
            - chrono::Local.timestamp_millis_opt(leap_start).unwrap())
        .num_days();
        assert_eq!(leap_days, 29, "Feb 2024 is a leap month");

        // 2025 is not a leap year: February has 28 days.
        let (start, end) = month_window_for(2025, 2, 10);
        assert_is_first_at_midnight(start, 2025, 2);
        assert_is_first_at_midnight(end, 2025, 3);
        let days = (chrono::Local.timestamp_millis_opt(end).unwrap()
            - chrono::Local.timestamp_millis_opt(start).unwrap())
        .num_days();
        assert_eq!(days, 28, "Feb 2025 is not a leap month");
    }

    #[test]
    fn local_month_window_start_is_local_midnight_across_a_dst_month() {
        // US DST springs forward in March; the month boundary must still land on
        // local midnight (the helper routes both ends through local_midnight_ms,
        // which probes past a skipped/ambiguous midnight). The day count tolerates
        // the 23h transition day.
        let (start, end) = month_window_for(2026, 3, 12);
        assert_is_first_at_midnight(start, 2026, 3);
        assert_is_first_at_midnight(end, 2026, 4);
    }

    // ---- priced-only windowed spend ----

    #[test]
    fn priced_spend_sums_only_priced_rows_and_counts_unpriced() {
        let (_dir, db) = test_db();
        // Two priced api_request rows: $1.50 + $0.50 = $2.00.
        insert_request(
            &db,
            Some("s"),
            START + 1,
            Some(1.5),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("s"),
            START + 2,
            Some(0.5),
            (1, 0, 0, 0),
            "api_request",
        );
        // Two unpriced api_request rows: excluded from the sum, counted instead.
        insert_request(&db, Some("s"), START + 3, None, (1, 0, 0, 0), "api_request");
        insert_request(&db, Some("s"), START + 4, None, (1, 0, 0, 0), "api_request");
        // An error row carries NULL cost but is not an api_request: neither summed
        // nor counted as unpriced.
        insert_request(&db, Some("s"), START + 5, None, (0, 0, 0, 0), "api_error");

        let (sum, unpriced) = priced_spend_for_window(&db, START, END, None).unwrap();
        assert_eq!(sum, 2.0);
        assert_eq!(unpriced, 2, "only NULL-cost api_request rows in window");
    }

    #[test]
    fn priced_spend_respects_window_bounds() {
        let (_dir, db) = test_db();
        // Just before the window: excluded.
        insert_request(
            &db,
            Some("s"),
            START - 1,
            Some(9.0),
            (1, 0, 0, 0),
            "api_request",
        );
        // At the inclusive start: included.
        insert_request(
            &db,
            Some("s"),
            START,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        // Inside: included.
        insert_request(
            &db,
            Some("s"),
            START + 1,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );
        // At the exclusive end: excluded.
        insert_request(&db, Some("s"), END, Some(4.0), (1, 0, 0, 0), "api_request");

        let (sum, unpriced) = priced_spend_for_window(&db, START, END, None).unwrap();
        assert_eq!(sum, 3.0);
        assert_eq!(unpriced, 0);
    }

    #[test]
    fn priced_spend_empty_window_is_zero_and_zero() {
        let (_dir, db) = test_db();
        assert_eq!(
            priced_spend_for_window(&db, START, END, None).unwrap(),
            (0.0, 0)
        );
    }

    #[test]
    fn priced_spend_event_time_floor_excludes_rows_before_the_floor() {
        let (_dir, db) = test_db();
        let floor = START + 1000;
        // Before the floor (the storm guard: pre-launch / recovered spend).
        insert_request(
            &db,
            Some("s"),
            START + 500,
            Some(5.0),
            (1, 0, 0, 0),
            "api_request",
        );
        // Before the floor and unpriced: must not count toward unpriced either.
        insert_request(
            &db,
            Some("s"),
            START + 600,
            None,
            (1, 0, 0, 0),
            "api_request",
        );
        // At the floor (inclusive) and after: counted.
        insert_request(
            &db,
            Some("s"),
            floor,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("s"),
            floor + 50,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("s"),
            floor + 60,
            None,
            (1, 0, 0, 0),
            "api_request",
        );

        let (sum, unpriced) = priced_spend_for_window(&db, START, END, Some(floor)).unwrap();
        assert_eq!(sum, 3.0, "only spend at/after the floor counts");
        assert_eq!(unpriced, 1, "only unpriced rows at/after the floor count");

        // No floor sees everything in the window.
        let (sum_all, unpriced_all) = priced_spend_for_window(&db, START, END, None).unwrap();
        assert_eq!(sum_all, 8.0);
        assert_eq!(unpriced_all, 2);
    }

    // ---- performance (popover <100ms budget, NFR) ----

    #[test]
    fn metrics_query_under_100ms_with_150k_rows() {
        let (_dir, db) = test_db();
        let newest = END - 1;

        // 150k requests over 30 days, 600 sessions across 12 projects;
        // roughly 5k rows land inside the queried day.
        db.conn().execute_batch("BEGIN").unwrap();
        for s in 0..600 {
            db.conn()
                .execute(
                    "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                     VALUES (?1, ?2, ?3)",
                    params![format!("sess-{s}"), format!("/proj/p{}", s % 12), START],
                )
                .unwrap();
        }
        {
            let mut stmt = db
                .conn()
                .prepare(
                    "INSERT INTO requests (
                        session_id, timestamp_ms, cost_usd, input_tokens,
                        output_tokens, cache_read_tokens, cache_creation_tokens
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .unwrap();
            for i in 0i64..150_000 {
                let ts = newest - (i * 17_280); // spread over ~30 days
                stmt.execute(params![
                    format!("sess-{}", i % 600),
                    ts,
                    0.01,
                    100,
                    50,
                    2000,
                    100
                ])
                .unwrap();
            }
        }
        db.conn().execute_batch("COMMIT").unwrap();

        let started = std::time::Instant::now();
        let metrics = metrics_for_window(&db, START, END).unwrap();
        let elapsed = started.elapsed();

        assert!(metrics.requests > 4_000, "seed must cover the day");
        assert_eq!(metrics.top_projects.len(), 3);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "metrics queries took {elapsed:?} against 150k rows"
        );
    }

    // ---- daily cost series (sparkline, task 4.3) ----

    /// Fixed boundaries for a 7-day series ending at the test day.
    fn seven_day_boundaries() -> Vec<i64> {
        (0..=7).map(|i| START - (6 - i) * DAY_MS).collect()
    }

    #[test]
    fn daily_series_buckets_match_per_day_aggregation_exactly() {
        let (_dir, db) = test_db();
        // Costs on days -6, -3 (two rows), and today; the rest are gaps.
        insert_request(
            &db,
            Some("a"),
            START - 6 * DAY_MS + 10,
            Some(1.25),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("b"),
            START - 3 * DAY_MS + 10,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("b"),
            START - 3 * DAY_MS + 20,
            Some(0.5),
            (1, 0, 0, 0),
            "api_request",
        );
        insert_request(
            &db,
            Some("c"),
            START + 10,
            Some(4.0),
            (1, 0, 0, 0),
            "api_request",
        );

        let series = daily_cost_series(&db, &seven_day_boundaries()).unwrap();
        assert_eq!(series.len(), 7);
        let costs: Vec<f64> = series.iter().map(|d| d.cost_usd).collect();
        assert_eq!(costs, vec![1.25, 0.0, 0.0, 2.5, 0.0, 0.0, 4.0]);
        let requests: Vec<i64> = series.iter().map(|d| d.requests).collect();
        assert_eq!(requests, vec![1, 0, 0, 2, 0, 0, 1]);

        // Every bucket equals the full metrics aggregation for its window:
        // the sparkline can never disagree with today_metrics.
        for (i, bucket) in series.iter().enumerate() {
            let window_start = START - (6 - i as i64) * DAY_MS;
            assert_eq!(bucket.day_start_ms, window_start);
            let full = metrics_for_window(&db, window_start, window_start + DAY_MS).unwrap();
            assert_eq!(bucket.cost_usd, full.cost_usd);
            assert_eq!(bucket.requests, full.requests);
        }
    }

    #[test]
    fn daily_series_day_boundaries_are_inclusive_start_exclusive_end() {
        let (_dir, db) = test_db();
        let day3_start = START - 3 * DAY_MS;
        // Exactly at a midnight boundary: belongs to the day it opens.
        insert_request(
            &db,
            Some("s"),
            day3_start,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );
        // One ms before that midnight: belongs to the previous day.
        insert_request(
            &db,
            Some("s"),
            day3_start - 1,
            Some(2.0),
            (1, 0, 0, 0),
            "api_request",
        );

        let series = daily_cost_series(&db, &seven_day_boundaries()).unwrap();
        let costs: Vec<f64> = series.iter().map(|d| d.cost_usd).collect();
        assert_eq!(costs, vec![0.0, 0.0, 2.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn daily_series_empty_db_yields_all_zero_buckets() {
        let (_dir, db) = test_db();
        let series = daily_cost_series(&db, &seven_day_boundaries()).unwrap();
        assert_eq!(series.len(), 7);
        assert!(series.iter().all(|d| d.cost_usd == 0.0 && d.requests == 0));
    }

    #[test]
    fn daily_series_single_active_day_keeps_explicit_gap_buckets() {
        let (_dir, db) = test_db();
        insert_request(
            &db,
            Some("s"),
            START + 10,
            Some(3.0),
            (1, 0, 0, 0),
            "api_request",
        );
        let series = daily_cost_series(&db, &seven_day_boundaries()).unwrap();
        assert_eq!(series.len(), 7, "gap days stay as explicit zero buckets");
        assert_eq!(series[6].cost_usd, 3.0);
        assert!(series[..6].iter().all(|d| d.cost_usd == 0.0));
    }

    #[test]
    fn daily_series_excludes_unpriced_cost_and_error_requests() {
        let (_dir, db) = test_db();
        // Unpriced row: no cost contribution, still a request.
        insert_request(&db, Some("s"), START + 1, None, (5, 0, 0, 0), "api_request");
        // Error row: neither cost nor request.
        insert_request(&db, Some("s"), START + 2, None, (0, 0, 0, 0), "api_error");
        insert_request(
            &db,
            Some("s"),
            START + 3,
            Some(1.0),
            (1, 0, 0, 0),
            "api_request",
        );

        let series = daily_cost_series(&db, &seven_day_boundaries()).unwrap();
        let today = series.last().unwrap();
        assert_eq!(today.cost_usd, 1.0);
        assert_eq!(today.requests, 2);
    }

    #[test]
    fn trailing_day_boundaries_are_local_midnights_ascending() {
        let now = chrono::Local::now();
        for days in [7u32, 30] {
            let boundaries = trailing_day_boundaries(days, now);
            assert_eq!(boundaries.len(), days as usize + 1);
            assert!(boundaries.windows(2).all(|w| w[0] < w[1]));
            // The last pair is exactly today's window.
            let (today_start, today_end) = local_day_window(now);
            assert_eq!(boundaries[days as usize - 1], today_start);
            assert_eq!(boundaries[days as usize], today_end);
            // Every boundary renders as a 00:00 local wall-clock time.
            use chrono::TimeZone;
            for ms in &boundaries {
                let local = chrono::Local.timestamp_millis_opt(*ms).unwrap();
                assert_eq!(local.format("%H:%M:%S%.3f").to_string(), "00:00:00.000");
            }
            // Each day is 24h except DST transitions (23h/25h).
            for w in boundaries.windows(2) {
                let len = w[1] - w[0];
                assert!(
                    (23 * 3_600_000..=25 * 3_600_000).contains(&len),
                    "day length out of range: {len}ms"
                );
            }
        }
    }

    #[test]
    fn daily_costs_command_clamps_days_and_reads_managed_db() {
        use std::sync::{Arc, Mutex};

        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        let now_ms = chrono::Local::now().timestamp_millis();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, input_tokens)
                 VALUES ('sess-now', ?1, 0.75, 42)",
                params![now_ms],
            )
            .unwrap();

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DbState(Arc::new(Mutex::new(db))));

        let series = daily_costs(app.handle().clone(), 7).expect("series");
        assert_eq!(series.len(), 7);
        assert_eq!(series.last().unwrap().cost_usd, 0.75);
        assert_eq!(series.last().unwrap().requests, 1);

        // days = 0 clamps to 1 bucket instead of an empty (or panicking) series.
        let clamped = daily_costs(app.handle().clone(), 0).expect("clamped series");
        assert_eq!(clamped.len(), 1);
        assert_eq!(clamped[0].cost_usd, 0.75);
    }

    #[test]
    fn daily_cost_serializes_for_frontend() {
        let bucket = DailyCost {
            day_start_ms: START,
            cost_usd: 1.5,
            requests: 3,
        };
        assert_eq!(
            serde_json::to_value(&bucket).unwrap(),
            serde_json::json!({"day_start_ms": START, "cost_usd": 1.5, "requests": 3})
        );
    }

    // ---- serialization contract for the frontend ----

    #[test]
    fn metrics_serialize_for_frontend() {
        let metrics = TodayMetrics {
            day_start_ms: START,
            day_end_ms: END,
            cost_usd: 12.5,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 2000,
            cache_creation_tokens: 300,
            requests: 7,
            unpriced_requests: 1,
            sessions: 2,
            top_projects: vec![ProjectCost {
                cwd: Some("/proj/a".into()),
                cost_usd: 12.5,
                requests: 7,
            }],
        };
        let value = serde_json::to_value(&metrics).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "day_start_ms": START,
                "day_end_ms": END,
                "cost_usd": 12.5,
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_tokens": 2000,
                "cache_creation_tokens": 300,
                "requests": 7,
                "unpriced_requests": 1,
                "sessions": 2,
                "top_projects": [
                    {"cwd": "/proj/a", "cost_usd": 12.5, "requests": 7}
                ],
            })
        );
    }

    // ---- command wiring over a real (mock-runtime) app ----

    #[test]
    fn today_metrics_command_reads_managed_db() {
        use std::sync::{Arc, Mutex};

        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        let now_ms = chrono::Local::now().timestamp_millis();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, input_tokens)
                 VALUES ('sess-now', ?1, 0.25, 42)",
                params![now_ms],
            )
            .unwrap();

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DbState(Arc::new(Mutex::new(db))));

        let metrics = today_metrics(app.handle().clone()).expect("metrics");
        assert_eq!(metrics.sessions, 1);
        assert_eq!(metrics.input_tokens, 42);
        assert_eq!(metrics.cost_usd, 0.25);
        assert!(metrics.day_start_ms <= now_ms && now_ms < metrics.day_end_ms);
    }
}
