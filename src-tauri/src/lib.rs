use std::sync::Mutex;

use tauri::Manager;

pub mod db;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // macOS: ~/Library/Application Support/com.peason.farthing
            let data_dir = app.path().app_data_dir()?;
            let database = db::Db::open_in_dir(&data_dir)?;
            app.manage(db::DbState(Mutex::new(database)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
