//! Headless harness for end-to-end pipeline verification (task 1.6).
//!
//! Runs the exact production stack — `Db::open_in_dir` + the receiver
//! router on `127.0.0.1:43177` — without the Tauri shell, so a real Claude
//! Code session can be pointed at it and the resulting `usage.db` inspected
//! with `sqlite3`. See `docs/notes/otel-schema.md` for the verification
//! procedure and findings.
//!
//! Usage:
//!
//! ```sh
//! cargo run --example e2e_receiver -- /path/to/data-dir
//! ```

use std::sync::{Arc, Mutex};

use claude_usage_tracker_lib::{db, ingest, receiver};

#[tokio::main]
async fn main() {
    let data_dir = std::env::args()
        .nth(1)
        .expect("usage: e2e_receiver <data-dir>");
    let database = db::Db::open_in_dir(std::path::Path::new(&data_dir))
        .unwrap_or_else(|err| panic!("failed to open database in {data_dir}: {err}"));
    let ingest_state = ingest::IngestState::new(Arc::new(Mutex::new(database)));

    let status = receiver::new_status();
    eprintln!(
        "e2e_receiver: db at {data_dir}/{}, binding 127.0.0.1:{}",
        db::DB_FILE_NAME,
        receiver::OTLP_PORT
    );
    receiver::run(Arc::clone(&status), ingest_state).await;
    eprintln!(
        "e2e_receiver: stopped: {:?}",
        status.lock().expect("status mutex poisoned")
    );
}
