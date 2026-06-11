use std::sync::{Arc, Mutex};

use tauri::Manager;

pub mod autostart;
pub mod db;
pub mod health;
pub mod ingest;
pub mod onboarding;
pub mod pricing;
pub mod receiver;
pub mod session;
pub mod settings_merge;
pub mod transcript;
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
        .setup(|app| {
            // macOS: ~/Library/Application Support/com.peason.farthing
            let data_dir = app.path().app_data_dir()?;
            let database = Arc::new(Mutex::new(db::Db::open_in_dir(&data_dir)?));
            app.manage(db::DbState(Arc::clone(&database)));

            // Ingest pipeline state: shared DB handle + counters, queryable
            // via `ingest_stats` (health view, task 2.5).
            let ingest_state = ingest::IngestState::new(database);
            app.manage(ingest_state.clone());

            // Pricing table for backfill cost computation (task 3.3):
            // bundled snapshot + local cache load synchronously; the remote
            // refresh is spawned fail-silent and never blocks startup.
            let pricing_state = pricing::PricingState::new(pricing::PricingTable::load(&data_dir));
            app.manage(pricing_state.clone());
            tauri::async_runtime::spawn(pricing::refresh(pricing_state, data_dir.clone()));

            // OTLP receiver on 127.0.0.1:43177. A port conflict is recorded
            // in ReceiverState (queryable via `receiver_status`), never
            // auto-rebound: settings.json holds the literal endpoint.
            let status = receiver::new_status();
            app.manage(receiver::ReceiverState(Arc::clone(&status)));
            tauri::async_runtime::spawn(receiver::run(status, ingest_state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            receiver::receiver_status,
            ingest::ingest_stats,
            health::health_status,
            onboarding::onboarding_status,
            onboarding::onboarding_apply,
            autostart::autostart_status,
            autostart::autostart_set,
            uninstall::uninstall_status,
            uninstall::uninstall_apply
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
