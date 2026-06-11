//! Headless backfill verification harness (task 3.4).
//!
//! Runs the production backfill engine against a real transcript tree
//! (read-only) into a database in a directory of your choosing, twice, to
//! demonstrate the full first pass and the idempotent re-run:
//!
//! ```sh
//! cargo run --example backfill_pass -- ~/.claude/projects /tmp/backfill-db
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};

use claude_usage_tracker_lib::backfill::{run_pass, BackfillState, BackfillSummary};
use claude_usage_tracker_lib::db::Db;
use claude_usage_tracker_lib::pricing::{PricingState, PricingTable};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .expect("usage: backfill_pass <projects-root> <db-dir>");
    let db_dir = args
        .next()
        .expect("usage: backfill_pass <projects-root> <db-dir>");
    let root = Path::new(&root);
    let db_dir = Path::new(&db_dir);

    let db = Arc::new(Mutex::new(Db::open_in_dir(db_dir).expect("open db")));
    let pricing = PricingState::new(PricingTable::load(db_dir));
    let state = BackfillState::default();

    let first = run_pass(&db, &pricing, &state, root);
    print_summary("first pass", &first);
    report(&db);

    let second = run_pass(&db, &pricing, &state, root);
    print_summary("second pass (must be a no-op)", &second);
    report(&db);
}

fn print_summary(label: &str, s: &BackfillSummary) {
    println!("== {label} ({} ms) ==", s.finished_ms - s.started_ms);
    println!(
        "files: {} discovered, {} read, {} reset, {} io errors",
        s.files_discovered, s.files_read, s.files_reset, s.io_errors
    );
    println!(
        "requests: {} seen, {} inserted, {} deduped, {} splits filled, {} unknown-model",
        s.requests_seen,
        s.requests_inserted,
        s.requests_deduped,
        s.splits_filled,
        s.unknown_model_rows
    );
    println!(
        "sessions: {} created, {} healed",
        s.sessions_created, s.sessions_healed
    );
    println!(
        "lines: {} read, {} assistant, {} skipped, {} malformed, {} invalid",
        s.parse.lines_read,
        s.parse.assistant_lines,
        s.parse.skipped_lines,
        s.parse.malformed_lines,
        s.parse.invalid_assistant_lines
    );
}

fn report(db: &Arc<Mutex<Db>>) {
    let db = db.lock().unwrap();
    let conn = db.conn();
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    println!(
        "db: {} requests ({} backfill, {} priced, {} null-cost), {} sessions ({} with cwd), \
         {} offsets, total cost ${:.2}",
        q("SELECT COUNT(*) FROM requests"),
        q("SELECT COUNT(*) FROM requests WHERE source = 'backfill'"),
        q("SELECT COUNT(*) FROM requests WHERE cost_usd IS NOT NULL"),
        q("SELECT COUNT(*) FROM requests WHERE cost_usd IS NULL"),
        q("SELECT COUNT(*) FROM sessions"),
        q("SELECT COUNT(*) FROM sessions WHERE cwd IS NOT NULL"),
        q("SELECT COUNT(*) FROM ingest_state"),
        conn.query_row("SELECT COALESCE(SUM(cost_usd), 0) FROM requests", [], |r| r
            .get::<_, f64>(0))
            .unwrap()
    );
    println!();
}
