use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

pub mod alerts;
pub mod autostart;
pub mod backfill;
pub mod capture;
pub mod db;
pub mod health;
pub mod ingest;
pub mod metrics;
pub mod notify;
pub mod onboarding;
pub mod pricing;
pub mod queries;
pub mod receiver;
pub mod session;
pub mod settings_merge;
pub mod transcript;
pub mod tray;
pub mod tray_title;
pub mod uninstall;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // LaunchAgent mode per PRD; enabled during onboarding, toggleable
        // on the settings view (autostart.rs).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // Native desktop notifications for cost alerts (notify.rs). Driven
        // from Rust via NotificationExt; display-only (no click handlers).
        .plugin(tauri_plugin_notification::init())
        // Anchors the popover window to the tray icon (tray.rs feeds it the
        // tray events it positions against).
        .plugin(tauri_plugin_positioner::init())
        // Popover click-away dismissal + main-window close → hide +
        // activation policy flip (tray.rs, task 4.1).
        .on_window_event(tray::handle_window_event)
        .setup(|app| {
            // macOS: ~/Library/Application Support/com.peason.farthing
            // Dev/test override: point the whole data dir (usage.db, pricing
            // cache) at a seeded directory, e.g. one produced by
            // `cargo run --example seed_metrics_db`, without touching the
            // real install's data.
            let data_dir = match std::env::var_os("FARTHING_DATA_DIR") {
                Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
                _ => {
                    let dir = app.path().app_data_dir()?;
                    // One-time rename migration (2026-06-12): the pre-Farthing
                    // identifier kept its data in a sibling directory. Move
                    // usage.db (+ WAL/SHM) over before the DB opens; failure
                    // is non-fatal (a fresh DB is created, old data stays put).
                    if let Some(parent) = dir.parent() {
                        let legacy = parent.join(db::LEGACY_DATA_DIR_NAME);
                        if let Err(err) = db::migrate_legacy_data_dir(&legacy, &dir) {
                            eprintln!("db: legacy data dir migration failed: {err}");
                        }
                    }
                    dir
                }
            };
            let database = Arc::new(Mutex::new(db::Db::open_in_dir(&data_dir)?));
            app.manage(db::DbState(Arc::clone(&database)));

            // Capture pause/resume (task 4.4): persisted in `meta`, so a
            // paused app stays paused across restarts. Loaded before the
            // receiver spawns and before tray::setup seeds the menu/badge.
            let capture_state = capture::CaptureState::load(Arc::clone(&database));
            app.manage(capture_state.clone());

            // Cost-alert config + runtime (cost-notifications plan): persisted
            // as JSON in `meta`, cached in memory. `load` also captures
            // `process_start_ms` (wall clock) so burst/delta can floor their
            // spend queries on launch and never fire on recovered pre-launch
            // spend. The engine (later units) reads/writes this under its own
            // eval lock; nothing here triggers an evaluation yet.
            app.manage(alerts::AlertState::load(Arc::clone(&database)));

            // Ingest pipeline state: shared DB handle + counters, queryable
            // via `ingest_stats` (health view, task 2.5). The receiver
            // consults the shared pause flag per request, and pushes a
            // Tauri event after each export that stores rows so the popover
            // updates live instead of polling (task 4.4).
            let ingest_app = app.handle().clone();
            let ingest_state = ingest::IngestState::new(Arc::clone(&database))
                .with_pause_flag(capture_state.pause_flag())
                .with_notifier(Arc::new(move |stored| {
                    let _ = ingest_app.emit(ingest::INGESTED_EVENT, stored);
                    // Tray title tracks today's cost; updated Rust-side so
                    // it never round-trips through the webview.
                    tray_title::refresh(&ingest_app);
                }));
            app.manage(ingest_state.clone());

            // Pricing table for backfill cost computation (task 3.3):
            // bundled snapshot + local cache load synchronously; the remote
            // refresh is spawned fail-silent and never blocks startup.
            let pricing_state = pricing::PricingState::new(pricing::PricingTable::load(&data_dir));
            app.manage(pricing_state.clone());
            tauri::async_runtime::spawn(pricing::refresh(pricing_state.clone(), data_dir.clone()));

            // Transcript backfill (task 3.4): one incremental pass on every
            // start (a fresh install's pass is the full-history pass —
            // every file starts at offset 0). Runs on a blocking thread:
            // file I/O + DB writes; dedup on request_id makes it safe to
            // run concurrently with live ingest.
            let backfill_state = backfill::BackfillState::default();
            app.manage(backfill_state.clone());
            let projects_root = backfill::projects_root(app.handle())?;
            let backfill_db = Arc::clone(&database);
            let backfill_pricing = pricing_state.clone();
            let backfill_app = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                backfill::run_pass(
                    &backfill_db,
                    &backfill_pricing,
                    &backfill_state,
                    &projects_root,
                );
                // The pass may have recovered rows from today; reflect them
                // in the tray title.
                tray_title::refresh(&backfill_app);
            });

            // OTLP receiver on 127.0.0.1:43177. A port conflict is recorded
            // in ReceiverState (queryable via `receiver_status`), never
            // auto-rebound: settings.json holds the literal endpoint.
            let status = receiver::new_status();
            app.manage(receiver::ReceiverState(Arc::clone(&status)));
            tauri::async_runtime::spawn(receiver::run(status, ingest_state));

            // Menu bar presence (task 4.1): tray icon + menu, popover shell,
            // ActivationPolicy::Accessory (no Dock icon until the desktop
            // window opens). Seeds the tray title (today's cost) too.
            tray::setup(app)?;

            // Coarse 60s tick keeping the tray title fresh: catches the
            // local-midnight rollover (cost resets to $0.00) without any
            // midnight-alarm bookkeeping. The query is an index-only range
            // scan; cheap enough to run for the life of the app.
            let tick_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                interval.tick().await; // first tick is immediate; already seeded
                loop {
                    interval.tick().await;
                    tray_title::refresh(&tick_app);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            receiver::receiver_status,
            ingest::ingest_stats,
            backfill::backfill_status,
            backfill::backfill_run,
            backfill::backfill_diff_report,
            health::health_status,
            metrics::today_metrics,
            metrics::daily_costs,
            queries::facet_options,
            queries::usage_summary,
            queries::usage_series,
            queries::session_rollups,
            queries::session_detail,
            queries::project_rollups,
            queries::home_dir,
            capture::capture_status,
            capture::capture_set_paused,
            notify::notification_permission_state,
            notify::notification_request_permission,
            notify::notification_send_test,
            alerts::alert_config_get,
            alerts::alert_config_set,
            onboarding::onboarding_status,
            onboarding::onboarding_apply,
            autostart::autostart_status,
            autostart::autostart_set,
            uninstall::uninstall_status,
            uninstall::uninstall_apply,
            tray::open_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
