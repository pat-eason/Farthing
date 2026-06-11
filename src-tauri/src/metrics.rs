//! Today-metrics queries for the menu bar popover (task 4.2).
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
//! Both queries are index-only range scans over `idx_requests_time_rollup`
//! (schema v3), so they touch only today's index pages regardless of table
//! size (<100ms popover budget, NFR).

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
fn local_midnight_ms(date: chrono::NaiveDate) -> i64 {
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

/// Frontend query: today's metrics for the popover.
#[tauri::command]
pub fn today_metrics<R: Runtime>(app: tauri::AppHandle<R>) -> Result<TodayMetrics, String> {
    let (day_start_ms, day_end_ms) = local_day_window(chrono::Local::now());
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    metrics_for_window(&db, day_start_ms, day_end_ms)
        .map_err(|err| format!("cannot query today's metrics: {err}"))
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
