//! Headless capture-completeness report verification (task 3.5).
//!
//! Runs a real backfill pass against a transcript tree (read-only) into a
//! scratch database, then exercises the diff report three ways:
//!
//! 1. all rows backfill (fresh DB): missing = 100%, matched = 0
//! 2. a slice of in-window rows flipped to `source='otel'` (simulating live
//!    capture): matched = flipped count, otel-only = 0
//! 3. one extra fabricated otel row with no transcript: otel-only = 1
//!
//! ```sh
//! cargo run --example diff_report -- ~/.claude/projects /tmp/diff-db
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};

use farthing_lib::backfill::{diff_report, run_pass, BackfillState, DiffReport};
use farthing_lib::db::Db;
use farthing_lib::pricing::{PricingState, PricingTable};

const WINDOW_HOURS: u32 = 24 * 7;
const FLIP_COUNT: i64 = 200;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .expect("usage: diff_report <projects-root> <db-dir>");
    let db_dir = args
        .next()
        .expect("usage: diff_report <projects-root> <db-dir>");
    let root = Path::new(&root);
    let db_dir = Path::new(&db_dir);

    let db = Arc::new(Mutex::new(Db::open_in_dir(db_dir).expect("open db")));
    let pricing = PricingState::new(PricingTable::load(db_dir));
    let state = BackfillState::default();

    let pass = run_pass(&db, &pricing, &state, root);
    println!(
        "backfill pass: {} files, {} requests inserted",
        pass.files_discovered, pass.requests_inserted
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    // 1. Everything backfill: nothing was captured live.
    let all_backfill = diff_report(&db, root, WINDOW_HOURS, now_ms).expect("report 1");
    print_report("all-backfill", &all_backfill);
    assert_eq!(all_backfill.matched, 0);
    assert_eq!(all_backfill.otel_only, 0);
    assert_eq!(all_backfill.backfill_only, all_backfill.transcript_requests);
    if all_backfill.transcript_requests > 0 {
        assert_eq!(all_backfill.missing_pct, Some(100.0));
    }

    // 2. Flip the newest in-window rows to otel: they must all match.
    let window_start_ms = now_ms - i64::from(WINDOW_HOURS) * 3_600_000;
    let flipped: i64 = {
        let db = db.lock().unwrap();
        db.conn()
            .execute(
                "UPDATE requests SET source = 'otel' WHERE request_id IN (
                    SELECT request_id FROM requests
                    WHERE timestamp_ms >= ?1 AND request_id IS NOT NULL
                    ORDER BY timestamp_ms DESC LIMIT ?2)",
                rusqlite::params![window_start_ms, FLIP_COUNT],
            )
            .unwrap() as i64
    };
    let with_live = diff_report(&db, root, WINDOW_HOURS, now_ms).expect("report 2");
    print_report(&format!("{flipped} rows flipped to otel"), &with_live);
    assert_eq!(with_live.matched, flipped as u64);
    assert_eq!(with_live.otel_only, 0);
    assert_eq!(
        with_live.backfill_only,
        with_live.transcript_requests - with_live.matched
    );

    // 3. A live row whose transcript no longer exists: otel-only.
    {
        let db = db.lock().unwrap();
        db.conn()
            .execute(
                "INSERT INTO requests (request_id, session_id, timestamp_ms, source)
                 VALUES ('req_diff_report_harness', 'sess-harness', ?1, 'otel')",
                [now_ms],
            )
            .unwrap();
    }
    let with_orphan = diff_report(&db, root, WINDOW_HOURS, now_ms).expect("report 3");
    print_report("plus one orphan otel row", &with_orphan);
    assert_eq!(with_orphan.otel_only, 1);
    assert_eq!(with_orphan.matched, with_live.matched);

    println!("all assertions passed");
}

fn print_report(label: &str, report: &DiffReport) {
    println!(
        "== {label} ==\n\
         window: last {}h (from {}), {} files scanned, {} io errors\n\
         transcript ground truth: {} requests\n\
         matched: {}, backfill-only: {}, otel-only: {}\n\
         missing: {}",
        report.window_hours,
        report.window_start_ms,
        report.files_scanned,
        report.io_errors,
        report.transcript_requests,
        report.matched,
        report.backfill_only,
        report.otel_only,
        report
            .missing_pct
            .map_or("n/a".to_string(), |pct| format!("{pct:.2}%")),
    );
}
