//! Menu bar tray title: today's cost rendered next to the tray icon.
//!
//! macOS renders a status item's title as text next to its icon, so the
//! tray permanently shows the current local day's API-equivalent cost
//! ("$12.34"), with the capture-paused badge folded in ("Paused · $12.34")
//! so pausing never hides the number.
//!
//! The cost matches the popover headline: `SUM(cost_usd)` over the
//! `[local midnight, next local midnight)` window from
//! [`crate::metrics::local_day_window`], so the day-boundary/DST rules live
//! in exactly one place. Unpriced rows (unknown model pricing) contribute
//! nothing — the popover breaks those out separately. The query is a single
//! SUM over the `idx_requests_facet_rollup` covering index (`timestamp_ms`
//! leads, `cost_usd` included): an index-only range scan cheap enough to
//! run every 60 seconds for the life of the app.
//!
//! [`refresh`] recomputes and sets the title. It is wired to every event
//! that can change today's cost or the pause badge:
//!
//! - app start, once the DB is ready (end of `tray::setup`)
//! - live ingest storing rows (the ingest notifier in `lib.rs`; Rust-side,
//!   no webview round-trip)
//! - a backfill pass completing (startup pass in `lib.rs`, manual pass in
//!   `backfill::backfill_run`)
//! - pause/resume (`capture::apply_paused`)
//! - a coarse 60s tick in `lib.rs` that catches the local-midnight rollover

use tauri::{AppHandle, Manager, Runtime};

use crate::capture::CaptureState;
use crate::db::{Db, DbState};

/// Menu-bar badge prefixed to the cost while capture is paused.
pub const PAUSED_BADGE: &str = "Paused";

/// Today's priced cost: `SUM(cost_usd)` over one `[start, end)` window.
/// Unpriced rows (`cost_usd IS NULL`) and `api_error` rows (NULL cost)
/// contribute nothing; an empty window is $0.00, not an error.
pub fn cost_for_window(db: &Db, day_start_ms: i64, day_end_ms: i64) -> rusqlite::Result<f64> {
    db.conn().query_row(
        "SELECT COALESCE(SUM(cost_usd), 0.0) FROM requests
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
        (day_start_ms, day_end_ms),
        |row| row.get(0),
    )
}

/// Render the tray title for a cost + pause state. Always two decimals
/// ("$0.00" with no data yet); the paused badge keeps the cost visible.
pub fn format_title(cost_usd: f64, paused: bool) -> String {
    let cost = format!("${:.2}", cost_usd.max(0.0));
    if paused {
        format!("{PAUSED_BADGE} · {cost}")
    } else {
        cost
    }
}

/// Recompute today's cost and set the tray title. Safe from any thread:
/// the query runs on the caller, the `set_title` is dispatched to the main
/// thread (tray mutations elsewhere are silently dropped, live-verified in
/// task 4.4). Tolerates missing managed state or a missing tray icon
/// (tests, startup ordering) — presentation only, never fails the caller.
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(db_state) = app.try_state::<DbState>() else {
        return;
    };
    let paused = app
        .try_state::<CaptureState>()
        .map(|state| state.paused())
        .unwrap_or(false);
    let (day_start_ms, day_end_ms) = crate::metrics::local_day_window(chrono::Local::now());
    let cost_usd = {
        let db = db_state.0.lock().expect("db mutex poisoned");
        match cost_for_window(&db, day_start_ms, day_end_ms) {
            Ok(cost) => cost,
            Err(err) => {
                // Keep the previous (correct-at-the-time) title rather than
                // showing a wrong $0.00; the next trigger retries anyway.
                eprintln!("tray title: cannot query today's cost: {err}");
                return;
            }
        }
    };
    let title = format_title(cost_usd, paused);
    let ui_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = ui_app.tray_by_id(crate::tray::TRAY_ID) {
            // Always a non-empty string, so the macOS set_title(None)
            // no-clear quirk (see task 4.4) can never bite here.
            let _ = tray.set_title(Some(title.as_str()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use rusqlite::params;
    use tempfile::TempDir;

    // ---- title formatting ----

    #[test]
    fn formats_zero_cost_as_zero_dollars() {
        assert_eq!(format_title(0.0, false), "$0.00");
    }

    #[test]
    fn formats_normal_cost_with_two_decimals() {
        assert_eq!(format_title(12.34, false), "$12.34");
        assert_eq!(format_title(7.0, false), "$7.00");
        assert_eq!(format_title(1.999, false), "$2.00");
    }

    #[test]
    fn paused_badge_keeps_the_cost_visible() {
        assert_eq!(format_title(12.34, true), "Paused · $12.34");
        assert_eq!(format_title(0.0, true), "Paused · $0.00");
    }

    #[test]
    fn negative_cost_clamps_to_zero() {
        // cost_usd should never be negative, but the title must not render
        // "$-0.01" if a bad row ever sneaks in.
        assert_eq!(format_title(-0.5, false), "$0.00");
    }

    // ---- today-cost query ----

    const DAY_MS: i64 = 86_400_000;
    const START: i64 = 1_781_150_400_000;
    const END: i64 = START + DAY_MS;

    fn test_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (dir, db)
    }

    fn insert_request(db: &Db, timestamp_ms: i64, cost_usd: Option<f64>, event_type: &str) {
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, cost_usd, event_type)
                 VALUES ('sess', ?1, ?2, ?3)",
                params![timestamp_ms, cost_usd, event_type],
            )
            .unwrap();
    }

    #[test]
    fn empty_db_costs_zero() {
        let (_dir, db) = test_db();
        assert_eq!(cost_for_window(&db, START, END).unwrap(), 0.0);
    }

    #[test]
    fn sums_priced_rows_inside_the_window_only() {
        let (_dir, db) = test_db();
        insert_request(&db, START - 1, Some(100.0), "api_request"); // yesterday
        insert_request(&db, START, Some(1.25), "api_request"); // inclusive start
        insert_request(&db, START + 1000, Some(2.0), "api_request");
        insert_request(&db, END, Some(100.0), "api_request"); // exclusive end

        assert_eq!(cost_for_window(&db, START, END).unwrap(), 3.25);
    }

    #[test]
    fn unpriced_and_error_rows_contribute_nothing() {
        let (_dir, db) = test_db();
        insert_request(&db, START + 1, Some(1.0), "api_request");
        insert_request(&db, START + 2, None, "api_request"); // unknown pricing
        insert_request(&db, START + 3, None, "api_error");

        assert_eq!(cost_for_window(&db, START, END).unwrap(), 1.0);
    }

    // ---- refresh tolerance (mock runtime) ----

    /// Presentation only: with no managed DB, then with a DB but no tray
    /// icon (mock runtime has none), refresh must silently no-op.
    #[test]
    fn refresh_tolerates_missing_state_and_tray() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        refresh(app.handle()); // no DbState managed

        let (_dir, db) = test_db();
        app.manage(DbState(Arc::new(Mutex::new(db))));
        refresh(app.handle()); // DB but no CaptureState and no tray
    }
}
