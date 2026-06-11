//! Seed a usage database for popover/desktop UI verification (task 4.2).
//!
//! Creates `usage.db` inside the given directory and fills it with requests
//! spread over the last 30 days (including today) plus matching sessions,
//! so the app can be pointed at realistic data volume via the
//! `CLAUDE_USAGE_TRACKER_DATA_DIR` override:
//!
//! ```sh
//! cargo run --example seed_metrics_db -- /tmp/seeded-data 120000
//! CLAUDE_USAGE_TRACKER_DATA_DIR=/tmp/seeded-data pnpm tauri dev
//! ```

use claude_usage_tracker_lib::db::Db;
use claude_usage_tracker_lib::metrics;

const DAY_MS: i64 = 86_400_000;
const DAYS: i64 = 30;
const SESSIONS: i64 = 600;
const PROJECTS: &[&str] = &[
    "/Users/dev/Projects/claude-usage-tracker",
    "/Users/dev/Projects/api-gateway",
    "/Users/dev/Projects/cms-service",
    "/Users/dev/Projects/presentations",
    "/Users/dev/Projects/node-framework",
    "/Users/dev/Projects/websites",
    "/Users/dev/Projects/tenant-service",
    "/Users/dev/Projects/audit-log-service",
    "/Users/dev/Projects/slide-service",
    "/Users/dev/Projects/async-jobs-sdk",
    "/Users/dev/Projects/feature-flags",
    "/Users/dev/Projects/design-system-ui",
];
const MODELS: &[(&str, f64)] = &[
    ("claude-sonnet-4-5-20250929", 0.018),
    ("claude-opus-4-5-20251101", 0.09),
    ("claude-haiku-4-5-20251001", 0.004),
];

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
    let now_ms = chrono::Local::now().timestamp_millis();

    db.conn().execute_batch("BEGIN").expect("begin");
    for s in 0..SESSIONS {
        db.conn()
            .execute(
                "INSERT OR IGNORE INTO sessions (session_id, cwd, first_seen_ms, source)
                 VALUES (?1, ?2, ?3, 'hook')",
                rusqlite::params![
                    format!("seed-sess-{s}"),
                    PROJECTS[(s % PROJECTS.len() as i64) as usize],
                    now_ms - DAYS * DAY_MS
                ],
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
                    cache_creation_tokens, source
                 ) VALUES (?1, ?2, ?3, ?4, 'user', ?5, ?6, ?7, ?8, ?9, 'otel')",
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
            let (model, base_cost) = MODELS[(i % MODELS.len() as i64) as usize];
            // ~1 in 500 rows unpriced (unknown model) to exercise that path.
            let cost: Option<f64> =
                (i % 500 != 0).then_some(base_cost * ((mix % 50) as f64) / 10.0);
            stmt.execute(rusqlite::params![
                format!("seed-req-{i}"),
                format!("seed-sess-{}", mix % SESSIONS),
                now_ms - age_ms,
                model,
                cost,
                20 + mix % 400,
                100 + mix % 2_000,
                mix % 80_000,
                mix % 8_000,
            ])
            .expect("insert request");
        }
    }
    db.conn().execute_batch("COMMIT").expect("commit");

    let (start, end) = metrics::local_day_window(chrono::Local::now());
    // Cold (first touch after open) and warm (page cache populated, the
    // resident app's steady state) query timings.
    let started = std::time::Instant::now();
    let _ = metrics::metrics_for_window(&db, start, end).expect("metrics");
    let cold = started.elapsed();
    let started = std::time::Instant::now();
    let today = metrics::metrics_for_window(&db, start, end).expect("metrics");
    let elapsed = started.elapsed();
    let total: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
        .expect("count");

    println!("seeded {total} requests into {dir}/usage.db");
    println!(
        "today: ${:.2}, {} requests, {} sessions, {} projects (query: cold {cold:?}, warm {elapsed:?})",
        today.cost_usd,
        today.requests,
        today.sessions,
        today.top_projects.len()
    );
}
