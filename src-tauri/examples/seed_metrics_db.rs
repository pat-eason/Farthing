//! Seed a usage database for popover/desktop UI verification (tasks 4.2,
//! 5.2) and check the faceted query layer against the Epic 5 perf budget.
//!
//! Creates `usage.db` inside the given directory and fills it with requests
//! spread over the trailing [`DAYS`] days (today-biased) plus matching
//! sessions, realistic for every facet the 5.2 query layer filters on:
//! multiple projects (including unknown-project flavors: NULL-cwd sessions
//! and requests whose session has no row at all), multiple models, a
//! main/subagent `query_source` mix, transcript-style rows carrying the
//! 5m/1h cache-creation split, occasional unpriced rows (unknown model
//! pricing) and `api_error` rows.
//!
//! After seeding it times every 5.2 aggregation (cold then warm) and exits
//! nonzero if any warm faceted query exceeds the 500ms Epic 5 budget, so
//! the 1M-row acceptance check is one command:
//!
//! ```sh
//! cargo run --release --example seed_metrics_db -- /tmp/seeded-data 1000000
//! CLAUDE_USAGE_TRACKER_DATA_DIR=/tmp/seeded-data pnpm tauri dev
//! ```

use claude_usage_tracker_lib::db::Db;
use claude_usage_tracker_lib::metrics;
use claude_usage_tracker_lib::queries::{
    self, Facets, ProjectFacet, QuerySourceFacet, RangeFacet, SeriesGroupBy, SessionSort,
};

const DAY_MS: i64 = 86_400_000;
const DAYS: i64 = 75;
const SESSIONS: i64 = 600;
const BUDGET_MS: u128 = 500;
const PROJECTS: &[&str] = &[
    "/Users/dev/Projects/farthing",
    "/Users/dev/Projects/api-server",
    "/Users/dev/Projects/content-service",
    "/Users/dev/Projects/frontend-app",
    "/Users/dev/Projects/api",
    "/Users/dev/Projects/web-app",
    "/Users/dev/Projects/core-service",
    "/Users/dev/Projects/log-service",
    "/Users/dev/Projects/media-service",
    "/Users/dev/Projects/task-sdk",
    "/Users/dev/Projects/config-service",
    "/Users/dev/Projects/ui-kit",
];
const MODELS: &[(&str, f64)] = &[
    ("claude-sonnet-4-5-20250929", 0.018),
    ("claude-opus-4-5-20251101", 0.09),
    ("claude-haiku-4-5-20251001", 0.004),
];

fn seed(db: &Db, rows: i64, now_ms: i64) {
    db.conn().execute_batch("BEGIN").expect("begin");
    for s in 0..SESSIONS {
        // Every 19th session has no cwd mapping: the "unknown project"
        // bucket the views must render as data, not errors (PRD FR-3).
        let cwd = (s % 19 != 0).then(|| PROJECTS[(s % PROJECTS.len() as i64) as usize]);
        db.conn()
            .execute(
                "INSERT OR IGNORE INTO sessions (session_id, cwd, first_seen_ms, source)
                 VALUES (?1, ?2, ?3, 'hook')",
                rusqlite::params![format!("seed-sess-{s}"), cwd, now_ms - DAYS * DAY_MS],
            )
            .expect("insert session");
    }
    {
        let mut stmt = db
            .conn()
            .prepare(
                "INSERT INTO requests (
                    request_id, session_id, timestamp_ms, model, query_source,
                    cost_usd, input_tokens, output_tokens, cache_read_tokens,
                    cache_creation_tokens, cache_creation_5m_tokens,
                    cache_creation_1h_tokens, event_type, source
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?14)",
            )
            .expect("prepare");
        for i in 0..rows {
            // Deterministic pseudo-random spread; bias ~1/8 of rows into the
            // last 24h so "today" has plenty of data.
            let mix = (i.wrapping_mul(2_654_435_761)) % 1_000_003;
            let age_ms = if i % 8 == 0 {
                mix % DAY_MS
            } else {
                DAY_MS + (mix * 37) % ((DAYS - 1) * DAY_MS)
            };
            // ~1/23 of rows reference a session with no sessions row at all:
            // the second unknown-project flavor.
            let session = if i % 23 == 0 {
                format!("seed-orphan-{}", mix % 100)
            } else {
                format!("seed-sess-{}", mix % SESSIONS)
            };
            let (model, base_cost) = MODELS[(i % MODELS.len() as i64) as usize];
            // Roughly the corpus mix: ~1/6 subagent (sidechain), the rest a
            // blend of otel-tagged 'user' and untagged (NULL) rows.
            let query_source = match i % 6 {
                0 => Some(queries::SUBAGENT_QUERY_SOURCE),
                1 | 2 => None,
                _ => Some("user"),
            };
            // ~1/400 rows are api_error events: no tokens, no cost.
            if i % 400 == 399 {
                stmt.execute(rusqlite::params![
                    format!("seed-req-{i}"),
                    session,
                    now_ms - age_ms,
                    model,
                    query_source,
                    Option::<f64>::None,
                    0,
                    0,
                    0,
                    0,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    "api_error",
                    "otel",
                ])
                .expect("insert error row");
                continue;
            }
            // ~1 in 500 rows unpriced (unknown model pricing).
            let cost: Option<f64> =
                (i % 500 != 0).then_some(base_cost * ((mix % 50) as f64) / 10.0);
            // Transcript-backfilled rows carry the 5m/1h split; otel rows
            // never do (the split is transcript-exclusive).
            let cache_creation = mix % 8_000;
            let (split_5m, split_1h, source) = if i % 4 == 0 {
                let five = (cache_creation * 3) / 4;
                (Some(five), Some(cache_creation - five), "backfill")
            } else {
                (None, None, "otel")
            };
            stmt.execute(rusqlite::params![
                format!("seed-req-{i}"),
                session,
                now_ms - age_ms,
                model,
                query_source,
                cost,
                20 + mix % 400,
                100 + mix % 2_000,
                mix % 80_000,
                cache_creation,
                split_5m,
                split_1h,
                "api_request",
                source,
            ])
            .expect("insert request");
        }
    }
    db.conn().execute_batch("COMMIT").expect("commit");
}

/// Run `query` twice and report (cold, warm) timings.
fn timed<T>(mut query: impl FnMut() -> T) -> (std::time::Duration, std::time::Duration) {
    let started = std::time::Instant::now();
    let _ = query();
    let cold = started.elapsed();
    let started = std::time::Instant::now();
    let _ = query();
    (cold, started.elapsed())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| {
        eprintln!("usage: seed_metrics_db <data-dir> [rows]");
        std::process::exit(2);
    });
    let rows: i64 = args
        .next()
        .map(|n| n.parse().expect("rows must be an integer"))
        .unwrap_or(120_000);

    let db = Db::open_in_dir(std::path::Path::new(&dir)).expect("open db");
    let now = chrono::Local::now();
    let now_ms = now.timestamp_millis();
    seed(&db, rows, now_ms);

    let total: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
        .expect("count");
    println!("seeded {total} requests into {dir}/usage.db");

    // Popover query (task 4.2 budget).
    let (start, end) = metrics::local_day_window(now);
    let (cold, warm) = timed(|| metrics::metrics_for_window(&db, start, end).expect("metrics"));
    let today = metrics::metrics_for_window(&db, start, end).expect("metrics");
    println!(
        "today: ${:.2}, {} requests, {} sessions, {} projects (query: cold {cold:?}, warm {warm:?})",
        today.cost_usd,
        today.requests,
        today.sessions,
        today.top_projects.len()
    );

    // Faceted query layer (task 5.2): every aggregation against the Epic 5
    // <500ms budget, both unfaceted worst cases and a fully faceted shape.
    let faceted = Facets {
        range: RangeFacet::Month,
        project: ProjectFacet::Cwd(PROJECTS[3].to_string()),
        model: Some(MODELS[0].0.to_string()),
        query_source: QuerySourceFacet::Main,
    };
    // Project-only is the worst faceted shape: nothing else prunes the
    // window scan before the session filter applies.
    let project_only = Facets {
        range: RangeFacet::Month,
        project: ProjectFacet::Cwd(PROJECTS[3].to_string()),
        ..Facets::default()
    };
    let unknown_project = Facets {
        range: RangeFacet::Month,
        project: ProjectFacet::Unknown,
        ..Facets::default()
    };
    let everything = Facets::default();
    let month = Facets {
        range: RangeFacet::Month,
        ..Facets::default()
    };
    let checks: Vec<(&str, (std::time::Duration, std::time::Duration))> = vec![
        (
            "summary (all rows, unfaceted)",
            timed(|| queries::summary_for(&db, &everything, now).expect("summary")),
        ),
        (
            "summary (month+project+model+source)",
            timed(|| queries::summary_for(&db, &faceted, now).expect("summary")),
        ),
        (
            "summary (month, project only)",
            timed(|| queries::summary_for(&db, &project_only, now).expect("summary")),
        ),
        (
            "summary (month, unknown project)",
            timed(|| queries::summary_for(&db, &unknown_project, now).expect("summary")),
        ),
        (
            "series (all days, ungrouped)",
            timed(|| {
                queries::series_for(&db, &everything, SeriesGroupBy::None, now).expect("series")
            }),
        ),
        (
            "series (faceted, grouped by model)",
            timed(|| {
                queries::series_for(&db, &faceted, SeriesGroupBy::Model, now).expect("series")
            }),
        ),
        (
            "series (month, grouped by project)",
            timed(|| {
                queries::series_for(&db, &month, SeriesGroupBy::Project, now).expect("series")
            }),
        ),
        (
            "sessions (month, cost desc, limit 200)",
            timed(|| {
                queries::session_rollups_for(&db, &month, SessionSort::Cost, true, 200, 0, now)
                    .expect("sessions")
            }),
        ),
        (
            "sessions (faceted)",
            timed(|| {
                queries::session_rollups_for(&db, &faceted, SessionSort::Cost, true, 200, 0, now)
                    .expect("sessions")
            }),
        ),
        (
            "projects (month)",
            timed(|| queries::project_rollups_for(&db, &month, now).expect("projects")),
        ),
        (
            "facet options",
            timed(|| queries::facet_options_for(&db).expect("options")),
        ),
    ];

    let mut over_budget = false;
    for (name, (cold, warm)) in &checks {
        let verdict = if warm.as_millis() < BUDGET_MS {
            "ok"
        } else {
            over_budget = true;
            "OVER BUDGET"
        };
        println!("faceted: {name}: cold {cold:?}, warm {warm:?} [{verdict}]");
    }
    if over_budget {
        eprintln!("FAIL: at least one warm faceted query exceeded {BUDGET_MS}ms on {total} rows");
        std::process::exit(1);
    }
    println!("all faceted queries under {BUDGET_MS}ms (warm) on {total} rows");
}
