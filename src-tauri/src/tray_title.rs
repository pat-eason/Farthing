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

/// Monochrome warning prefix: U+26A0 plus the text variation selector
/// (U+FE0E) so macOS renders the flat glyph, not the colored emoji.
const WARN_PREFIX: &str = "⚠\u{FE0E} ";

/// Pipe separating the cost from the budget readout (` | `).
const BUDGET_SEPARATOR: &str = " | ";

/// Stoplight dot for a band. Colored emoji circles render in color in the
/// menu bar (a plain-string approximation of a status dot — no native
/// attributed-string tinting needed): green / yellow / orange / red.
fn band_dot(band: crate::budgets::Band) -> &'static str {
    use crate::budgets::Band;
    match band {
        Band::Green => "🟢",
        Band::Yellow => "🟡",
        Band::Amber => "🟠",
        Band::Red => "🔴",
    }
}

/// Render the tray title with the optional budget readout folded in.
///
/// Starts from [`format_title`] (cost + pause badge). When `status` is
/// present and `show_in_tray` is set with at least one budget line, appends
/// ` | ` then each budget as `{dot} D {pct}%` / `{dot} M {pct}%` (daily
/// before monthly, only the present ones), where `{dot}` is the band's
/// stoplight glyph. Independently, when the worst band is Amber or Red,
/// prepends the monochrome warning glyph — even with `show_in_tray` off.
/// With no status, or nothing extra to show, returns the plain title.
pub fn format_budget_title(
    cost_usd: f64,
    paused: bool,
    status: Option<&crate::budgets::BudgetStatus>,
) -> String {
    use crate::budgets::Band;

    let base = format_title(cost_usd, paused);
    let Some(status) = status else {
        return base;
    };

    let warn = matches!(status.worst_band, Band::Amber | Band::Red);

    let mut budgets: Vec<String> = Vec::new();
    if status.show_in_tray {
        if let Some(daily) = status.daily.as_ref() {
            budgets.push(format!("{} D {}%", band_dot(daily.band), daily.percent));
        }
        if let Some(monthly) = status.monthly.as_ref() {
            budgets.push(format!("{} M {}%", band_dot(monthly.band), monthly.percent));
        }
    }

    let mut title = String::new();
    if warn {
        title.push_str(WARN_PREFIX);
    }
    title.push_str(&base);
    if !budgets.is_empty() {
        title.push_str(BUDGET_SEPARATOR);
        title.push_str(&budgets.join("  "));
    }
    title
}

/// Build the tray title for Subscription Mode.
///
/// Returns `None` when:
/// - mode is `Api` (caller falls through to the API path), or
/// - no snapshot is available yet (first launch, or disabled poller), or
/// - the snapshot has no window with a non-null utilization (server returned nulls).
///
/// When `Some`, the title format is `{warn?}{label_short} {pct:.0}% · ${cost}`,
/// e.g. `"5h 4% · $0.12"` or `"⚠\u{FE0E} 5h 92% · $1.23"`.
/// The paused badge is prepended if `paused` is true.
fn subscription_title(
    cost_usd: f64,
    paused: bool,
    display_mode: &crate::usage_limits::DisplayMode,
    snapshot: Option<&crate::usage_limits::UsageSnapshot>,
) -> Option<String> {
    if *display_mode != crate::usage_limits::DisplayMode::Subscription {
        return None;
    }
    let snapshot = snapshot?;

    let windows = [
        &snapshot.five_hour,
        &snapshot.seven_day,
        &snapshot.seven_day_sonnet,
        &snapshot.seven_day_opus,
    ];

    // Pick the most-utilized window with a non-null percent.
    let best = windows
        .iter()
        .filter(|w| w.percent.is_some())
        .max_by(|a, b| {
            a.percent
                .unwrap_or(0.0)
                .partial_cmp(&b.percent.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;

    let pct = best.percent?;

    // Near-limit warning: any window over 75%.
    let any_near_limit = windows.iter().any(|w| w.percent.unwrap_or(0.0) > 75.0);

    // Short label for the tray (space is limited).
    let label_short = match best.label.as_str() {
        "5h session" => "5h",
        "7d overall" => "7d",
        "7d Sonnet" => "7d·S",
        "7d Opus" => "7d·O",
        other => other,
    };

    let cost = format!("${:.2}", cost_usd.max(0.0));
    let usage_str = format!("{} {:.0}%", label_short, pct);

    let mut title = String::new();
    if any_near_limit {
        title.push_str(WARN_PREFIX);
    }
    if paused {
        title.push_str(PAUSED_BADGE);
        title.push_str(" · ");
    }
    title.push_str(&usage_str);
    title.push_str(" · ");
    title.push_str(&cost);
    Some(title)
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

    // Read display mode from config (brief DB lock).
    let display_mode = crate::usage_limits::read_config(&db_state).display_mode;

    // Read current usage snapshot from managed state (falls back to None if
    // the state isn't managed yet, e.g. on startup before lib.rs runs manage).
    let usage_snapshot: Option<crate::usage_limits::UsageSnapshot> = app
        .try_state::<crate::usage_limits::UsageLimitsState>()
        .and_then(|state| state.read().ok().map(|guard| guard.clone()))
        .flatten();
    let paused = app
        .try_state::<CaptureState>()
        .map(|state| state.paused())
        .unwrap_or(false);
    // Optional: a budget readout the formatter folds into the title. Missing
    // managed state (tests, startup ordering) => behave exactly as today.
    let budget_config = app
        .try_state::<crate::budgets::BudgetState>()
        .map(|state| state.config());
    let now = chrono::Local::now();
    let (day_start_ms, day_end_ms) = crate::metrics::local_day_window(now);
    let (cost_usd, status) = {
        let db = db_state.0.lock().expect("db mutex poisoned");
        let cost_usd = match cost_for_window(&db, day_start_ms, day_end_ms) {
            Ok(cost) => cost,
            Err(err) => {
                // Keep the previous (correct-at-the-time) title rather than
                // showing a wrong $0.00; the next trigger retries anyway.
                eprintln!("tray title: cannot query today's cost: {err}");
                return;
            }
        };
        let status = budget_config.as_ref().and_then(|config| {
            match crate::budgets::evaluate(&db, config, now) {
                Ok(status) => Some(status),
                Err(err) => {
                    // Budget readout is best-effort: drop it and still render
                    // the cost rather than failing the whole title refresh.
                    eprintln!("tray title: cannot evaluate budgets: {err}");
                    None
                }
            }
        });
        (cost_usd, status)
    };
    // macOS: when budgets show in the tray, draw the stacked readout as a
    // status-button image (set_title can't stack); otherwise restore the bird
    // icon and show the plain cost. Other platforms always use the title.
    #[cfg(target_os = "macos")]
    {
        // Subscription Mode: window-utilization primary, cost secondary.
        // Takes over the title before the budget-image path so both can coexist
        // with budget warning still prepended by subscription_title's near-limit
        // check. If subscription_title returns None we fall through to the
        // existing API-mode / budget-image path unchanged.
        if let Some(sub_title) =
            subscription_title(cost_usd, paused, &display_mode, usage_snapshot.as_ref())
        {
            let ui_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(tray) = ui_app.tray_by_id(crate::tray::TRAY_ID) {
                    // Restore the template icon if a drawn budget image was active.
                    if CUSTOM_IMAGE_ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        let _ = tray.set_icon_as_template(true);
                        let _ = tray.set_icon(Some(crate::tray::template_icon()));
                    }
                    let _ = tray.set_title(Some(sub_title.as_str()));
                }
            });
            return;
        }
        if let Some(model) = budget_render_model(cost_usd, paused, status.as_ref()) {
            // Render off the title path: draw a PNG and install it via
            // set_icon (which keeps tray-icon's click overlay sized to the
            // button, so left-click still toggles the popover).
            let fallback = format_budget_title(cost_usd, paused, status.as_ref());
            let ui_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                let Some(tray) = ui_app.tray_by_id(crate::tray::TRAY_ID) else {
                    return;
                };
                let png = objc2::MainThreadMarker::new()
                    .and_then(|mtm| crate::tray_render::render_png(&model, mtm));
                if let Some(img) =
                    png.and_then(|bytes| tauri::image::Image::from_bytes(&bytes).ok())
                {
                    let _ = tray.set_icon_as_template(false);
                    let _ = tray.set_icon(Some(img));
                    // Cost lives in the drawn image; clear the text title.
                    let _ = tray.set_title(Some(""));
                    CUSTOM_IMAGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
                } else {
                    // Rendering failed: degrade to the single-line title.
                    let _ = tray.set_title(Some(fallback.as_str()));
                }
            });
            return;
        }
        // Plain mode: restore the bird template icon if a drawn image replaced
        // it, then show the plain cost as the title.
        let plain = format_title(cost_usd, paused);
        let ui_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(tray) = ui_app.tray_by_id(crate::tray::TRAY_ID) {
                if CUSTOM_IMAGE_ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let _ = tray.set_icon_as_template(true);
                    let _ = tray.set_icon(Some(crate::tray::template_icon()));
                }
                let _ = tray.set_title(Some(plain.as_str()));
            }
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        let title = if let Some(sub_title) =
            subscription_title(cost_usd, paused, &display_mode, usage_snapshot.as_ref())
        {
            sub_title
        } else {
            format_budget_title(cost_usd, paused, status.as_ref())
        };
        let ui_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(tray) = ui_app.tray_by_id(crate::tray::TRAY_ID) {
                // Always a non-empty string, so the macOS set_title(None)
                // no-clear quirk (see task 4.4) can never bite here.
                let _ = tray.set_title(Some(title.as_str()));
            }
        });
    }
}

/// True while the macOS status button shows a drawn budget image instead of
/// the bird icon; lets [`refresh`] restore the icon on return to plain mode.
#[cfg(target_os = "macos")]
static CUSTOM_IMAGE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Build the stacked-readout model when budgets should show in the tray.
/// `None` => plain mode (no budgets set, or the tray toggle is off).
#[cfg(target_os = "macos")]
fn budget_render_model(
    cost_usd: f64,
    paused: bool,
    status: Option<&crate::budgets::BudgetStatus>,
) -> Option<crate::tray_render::Model> {
    let status = status?;
    if !status.show_in_tray {
        return None;
    }
    let mut rows = Vec::new();
    if let Some(daily) = status.daily.as_ref() {
        rows.push(crate::tray_render::Row {
            marker: "D",
            percent: daily.percent,
            band: daily.band,
        });
    }
    if let Some(monthly) = status.monthly.as_ref() {
        rows.push(crate::tray_render::Row {
            marker: "M",
            percent: monthly.percent,
            band: monthly.band,
        });
    }
    if rows.is_empty() {
        return None;
    }
    Some(crate::tray_render::Model {
        cost: format_title(cost_usd, paused),
        rows,
    })
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

    // ---- budget title formatting ----

    use crate::budgets::{Band, BudgetLine, BudgetStatus};

    /// A budget line at a given rounded percent / band; spend/amount values
    /// don't affect the title, so they're placeholders.
    fn line(percent: i64, band: Band) -> BudgetLine {
        BudgetLine {
            amount_usd: 100.0,
            spent_priced_usd: percent as f64,
            unpriced_requests: 0,
            percent,
            band,
            exceeded: false,
        }
    }

    fn status(
        daily: Option<BudgetLine>,
        monthly: Option<BudgetLine>,
        show_in_tray: bool,
        worst_band: Band,
    ) -> BudgetStatus {
        BudgetStatus {
            daily,
            monthly,
            show_in_tray,
            worst_band,
        }
    }

    #[test]
    fn no_status_returns_plain_title() {
        assert_eq!(format_budget_title(12.34, false, None), "$12.34");
    }

    #[test]
    fn both_under_amber_shown_appends_percents_no_warn() {
        let s = status(
            Some(line(40, Band::Green)),
            Some(line(20, Band::Green)),
            true,
            Band::Green,
        );
        assert_eq!(
            format_budget_title(12.34, false, Some(&s)),
            "$12.34 | 🟢 D 40%  🟢 M 20%"
        );
    }

    #[test]
    fn daily_amber_prepends_warn() {
        let s = status(
            Some(line(80, Band::Amber)),
            Some(line(20, Band::Green)),
            true,
            Band::Amber,
        );
        assert_eq!(
            format_budget_title(12.34, false, Some(&s)),
            "⚠\u{FE0E} $12.34 | 🟠 D 80%  🟢 M 20%"
        );
    }

    #[test]
    fn warn_without_show_in_tray_drops_percents() {
        // Red worst band warns even with the tray readout off, but no percents.
        let s = status(Some(line(120, Band::Red)), None, false, Band::Red);
        assert_eq!(
            format_budget_title(12.34, false, Some(&s)),
            "⚠\u{FE0E} $12.34"
        );
    }

    #[test]
    fn no_budget_lines_returns_plain_title() {
        let s = status(None, None, true, Band::Green);
        assert_eq!(format_budget_title(12.34, false, Some(&s)), "$12.34");
    }

    #[test]
    fn paused_with_amber_budget_keeps_badge_and_percents() {
        let s = status(
            Some(line(95, Band::Amber)),
            Some(line(20, Band::Green)),
            true,
            Band::Amber,
        );
        assert_eq!(
            format_budget_title(12.34, true, Some(&s)),
            "⚠\u{FE0E} Paused · $12.34 | 🟠 D 95%  🟢 M 20%"
        );
    }

    #[test]
    fn only_monthly_set_appends_just_monthly() {
        let s = status(None, Some(line(25, Band::Green)), true, Band::Green);
        assert_eq!(
            format_budget_title(12.34, false, Some(&s)),
            "$12.34 | 🟢 M 25%"
        );
    }

    #[test]
    fn only_monthly_amber_tray_off_warns_without_percents() {
        let s = status(None, Some(line(80, Band::Amber)), false, Band::Amber);
        assert_eq!(
            format_budget_title(12.34, false, Some(&s)),
            "⚠\u{FE0E} $12.34"
        );
    }

    // ---- subscription_title ----

    use crate::usage_limits::{DisplayMode, UsageSnapshot, UsageStatus, WindowSnapshot};

    fn window(label: &str, percent: Option<f64>) -> WindowSnapshot {
        WindowSnapshot {
            label: label.to_string(),
            percent,
            resets_at_ms: None,
        }
    }

    fn snapshot(five_pct: Option<f64>, seven_pct: Option<f64>) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: window("5h session", five_pct),
            seven_day: window("7d overall", seven_pct),
            seven_day_sonnet: window("7d Sonnet", Some(0.0)),
            seven_day_opus: window("7d Opus", Some(0.0)),
            extra_usage: None,
            fetched_at_ms: 0,
            status: UsageStatus::Ok,
        }
    }

    #[test]
    fn api_mode_returns_none() {
        let snap = snapshot(Some(4.0), Some(0.0));
        assert!(subscription_title(1.0, false, &DisplayMode::Api, Some(&snap)).is_none());
    }

    #[test]
    fn subscription_no_snapshot_returns_none() {
        assert!(subscription_title(1.0, false, &DisplayMode::Subscription, None).is_none());
    }

    #[test]
    fn subscription_formats_most_utilized_window() {
        let snap = snapshot(Some(4.0), Some(20.0));
        let title =
            subscription_title(1.23, false, &DisplayMode::Subscription, Some(&snap)).unwrap();
        assert_eq!(title, "7d 20% · $1.23");
    }

    #[test]
    fn subscription_near_limit_prepends_warn() {
        let snap = snapshot(Some(92.0), Some(0.0));
        let title =
            subscription_title(1.23, false, &DisplayMode::Subscription, Some(&snap)).unwrap();
        assert!(title.starts_with("⚠"), "expected warn prefix, got: {title}");
        assert!(title.contains("5h 92%"), "got: {title}");
    }

    #[test]
    fn subscription_paused_includes_badge() {
        let snap = snapshot(Some(4.0), Some(0.0));
        let title = subscription_title(0.0, true, &DisplayMode::Subscription, Some(&snap)).unwrap();
        assert!(title.contains("Paused"), "got: {title}");
    }

    #[test]
    fn subscription_all_null_percents_returns_none() {
        let snap = UsageSnapshot {
            five_hour: window("5h session", None),
            seven_day: window("7d overall", None),
            seven_day_sonnet: window("7d Sonnet", None),
            seven_day_opus: window("7d Opus", None),
            extra_usage: None,
            fetched_at_ms: 0,
            status: UsageStatus::Ok,
        };
        assert!(subscription_title(1.0, false, &DisplayMode::Subscription, Some(&snap)).is_none());
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
