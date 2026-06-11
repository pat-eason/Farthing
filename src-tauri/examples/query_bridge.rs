//! Dev bridge: serves the faceted query commands (task 5.2) over plain HTTP
//! so the analysis views (tasks 5.3-5.6) can be exercised in a normal
//! browser against a seeded database, where the Tauri IPC doesn't exist.
//!
//! Pair with `pnpm dev` and a `window.__TAURI_INTERNALS__.invoke` shim that
//! POSTs `JSON args` to `/invoke/<command>`:
//!
//! ```js
//! window.__TAURI_INTERNALS__ = {
//!   invoke: async (cmd, args) => {
//!     const res = await fetch(`http://127.0.0.1:43199/invoke/${cmd}`, {
//!       method: "POST",
//!       headers: { "content-type": "application/json" },
//!       body: JSON.stringify(args ?? {}),
//!     });
//!     if (!res.ok) throw await res.text();
//!     return res.json();
//!   },
//! };
//! ```
//!
//! Argument keys are camelCase exactly as `@tauri-apps/api` sends them, and
//! every command runs the same `queries::*_for` functions as production, so
//! what the browser renders is what the real webview would.
//!
//! Usage:
//!
//! ```sh
//! cargo run --example seed_metrics_db -- /tmp/seeded-data 150000
//! cargo run --example query_bridge -- /tmp/seeded-data [port]
//! ```

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde::Deserialize;

use claude_usage_tracker_lib::db::Db;
use claude_usage_tracker_lib::queries::{self, Facets, SeriesGroupBy, SessionSort};

const DEFAULT_PORT: u16 = 43199;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct InvokeArgs {
    facets: Facets,
    group_by: Option<SeriesGroupBy>,
    sort: Option<SessionSort>,
    descending: Option<bool>,
    limit: Option<u32>,
    offset: Option<u32>,
    session_id: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let data_dir = std::env::args()
        .nth(1)
        .expect("usage: query_bridge <data-dir> [port]");
    let port: u16 = std::env::args()
        .nth(2)
        .map(|raw| raw.parse().expect("port must be a number"))
        .unwrap_or(DEFAULT_PORT);
    let db = Db::open_in_dir(std::path::Path::new(&data_dir))
        .unwrap_or_else(|err| panic!("failed to open database in {data_dir}: {err}"));
    let db = Arc::new(Mutex::new(db));

    let app = Router::new()
        .route("/invoke/{command}", any(invoke))
        .with_state(db);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind bridge port");
    eprintln!("query_bridge: serving {data_dir} on http://{addr}/invoke/<command>");
    axum::serve(listener, app).await.expect("serve");
}

async fn invoke(
    method: Method,
    Path(command): Path<String>,
    State(db): State<Arc<Mutex<Db>>>,
    body: String,
) -> Response {
    if method == Method::OPTIONS {
        return cors(StatusCode::NO_CONTENT, String::new());
    }
    let args: InvokeArgs = match serde_json::from_str(if body.is_empty() { "{}" } else { &body }) {
        Ok(args) => args,
        Err(err) => return cors(StatusCode::BAD_REQUEST, format!("bad args: {err}")),
    };
    let db = db.lock().expect("db mutex poisoned");
    let now = chrono::Local::now();
    let result = match command.as_str() {
        "usage_summary" => to_json(queries::summary_for(&db, &args.facets, now)),
        "usage_series" => to_json(queries::series_for(
            &db,
            &args.facets,
            args.group_by.unwrap_or_default(),
            now,
        )),
        "session_rollups" => to_json(queries::session_rollups_for(
            &db,
            &args.facets,
            args.sort.unwrap_or_default(),
            args.descending.unwrap_or(true),
            args.limit.unwrap_or(200),
            args.offset.unwrap_or(0),
            now,
        )),
        "session_detail" => match &args.session_id {
            Some(session_id) => to_json(queries::session_detail_for(
                &db,
                session_id,
                &args.facets,
                queries::DETAIL_REQUEST_LIMIT,
                now,
            )),
            None => return cors(StatusCode::BAD_REQUEST, "missing sessionId".into()),
        },
        "project_rollups" => to_json(queries::project_rollups_for(&db, &args.facets, now)),
        "facet_options" => to_json(queries::facet_options_for(&db)),
        other => return cors(StatusCode::NOT_FOUND, format!("unknown command: {other}")),
    };
    match result {
        Ok(json) => cors(StatusCode::OK, json),
        Err(err) => cors(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

fn to_json<T: serde::Serialize>(result: Result<T, rusqlite::Error>) -> Result<String, String> {
    let value = result.map_err(|err| format!("query failed: {err}"))?;
    serde_json::to_string(&value).map_err(|err| format!("serialize failed: {err}"))
}

/// The vite dev server origin differs from the bridge's: every response
/// needs permissive CORS headers (dev tool, localhost only).
fn cors(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            ),
            (
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("POST, OPTIONS"),
            ),
            (
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("content-type"),
            ),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
        ],
        body,
    )
        .into_response()
}
