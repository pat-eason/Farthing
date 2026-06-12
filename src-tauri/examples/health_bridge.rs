//! Dev bridge: serves `health_status` (and `onboarding_status`) over plain
//! HTTP so every failure state from the task 6.4 error sweep can be
//! exercised in the real `/health` and onboarding views in a normal
//! browser, where the Tauri IPC doesn't exist.
//!
//! Each scenario is assembled through the production
//! [`health::compute_health`] (not hand-written JSON), so what the browser
//! renders is exactly what the real webview would for those inputs.
//! `onboarding_status` runs the production [`onboarding::compute_status`]
//! against a real settings file you control, so unreadable/malformed
//! settings.json can be exercised end to end (`chmod 000` it, reload,
//! `chmod 644` it, hit "Try again").
//!
//! Pair with `pnpm dev` and the same `window.__TAURI_INTERNALS__.invoke`
//! shim documented in `query_bridge.rs`.
//!
//! Usage:
//!
//! ```sh
//! cargo run --example health_bridge -- /tmp/health-sandbox/settings.json [port]
//! curl http://127.0.0.1:43198/scenario            # list scenarios
//! curl http://127.0.0.1:43198/scenario/db_locked  # switch the active one
//! ```

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::json;

use claude_usage_tracker_lib::backfill::BackfillInfo;
use claude_usage_tracker_lib::health::{
    compute_health, ConfigState, HealthStatus, StoredEvents, TranscriptsInfo,
};
use claude_usage_tracker_lib::ingest::IngestStatsSnapshot;
use claude_usage_tracker_lib::onboarding;
use claude_usage_tracker_lib::receiver::ReceiverStatus;
use claude_usage_tracker_lib::settings_merge::{describe_settings_error, detect_conflicts};

const DEFAULT_PORT: u16 = 43198;

/// Every state the 6.4 error sweep produced, in display order.
const SCENARIOS: [&str; 9] = [
    "healthy",
    "fresh_machine",
    "db_locked",
    "disk_full",
    "port_conflict",
    "receiver_failed",
    "paused",
    "config_unreadable",
    "config_conflicting",
];

struct Bridge {
    scenario: Mutex<String>,
    settings_path: PathBuf,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let settings_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: health_bridge <settings.json path> [port]"),
    );
    let port: u16 = std::env::args()
        .nth(2)
        .map(|raw| raw.parse().expect("port must be a number"))
        .unwrap_or(DEFAULT_PORT);
    let bridge = Arc::new(Bridge {
        scenario: Mutex::new("healthy".to_string()),
        settings_path,
    });

    let app = Router::new()
        .route("/invoke/{command}", any(invoke))
        .route("/scenario", any(list_scenarios))
        .route("/scenario/{name}", any(set_scenario))
        .with_state(bridge);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind bridge port");
    eprintln!("health_bridge: http://{addr}/invoke/<command>; scenarios at /scenario/<name>");
    axum::serve(listener, app).await.expect("serve");
}

async fn list_scenarios(State(bridge): State<Arc<Bridge>>) -> Response {
    let active = bridge.scenario.lock().expect("scenario mutex").clone();
    cors(
        StatusCode::OK,
        serde_json::to_string(&json!({ "active": active, "available": SCENARIOS })).unwrap(),
    )
}

async fn set_scenario(Path(name): Path<String>, State(bridge): State<Arc<Bridge>>) -> Response {
    if !SCENARIOS.contains(&name.as_str()) {
        return cors(
            StatusCode::NOT_FOUND,
            format!("unknown scenario: {name}; one of {SCENARIOS:?}"),
        );
    }
    *bridge.scenario.lock().expect("scenario mutex") = name.clone();
    cors(StatusCode::OK, format!("{{\"active\": \"{name}\"}}"))
}

async fn invoke(
    method: Method,
    Path(command): Path<String>,
    State(bridge): State<Arc<Bridge>>,
    body: String,
) -> Response {
    if method == Method::OPTIONS {
        return cors(StatusCode::NO_CONTENT, String::new());
    }
    match command.as_str() {
        "health_status" => {
            let scenario = bridge.scenario.lock().expect("scenario mutex").clone();
            let health = build(&scenario, &bridge.settings_path);
            cors(StatusCode::OK, serde_json::to_string(&health).unwrap())
        }
        // The production read-only onboarding computation against the real
        // (sandbox) file, so unreadable/malformed files hit the real path.
        "onboarding_status" => match onboarding::compute_status(&bridge.settings_path) {
            Ok(status) => cors(StatusCode::OK, serde_json::to_string(&status).unwrap()),
            Err(message) => cors(StatusCode::INTERNAL_SERVER_ERROR, message),
        },
        // The health view's "Resume capture" button: flip back to healthy
        // so the click visibly does something.
        "capture_set_paused" => {
            let paused = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("paused").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            if !paused {
                *bridge.scenario.lock().expect("scenario mutex") = "healthy".to_string();
            }
            cors(
                StatusCode::OK,
                serde_json::to_string(&json!({ "paused": paused })).unwrap(),
            )
        }
        other => cors(StatusCode::NOT_FOUND, format!("unknown command: {other}")),
    }
}

/// Assemble the scenario through the production `compute_health`.
fn build(scenario: &str, settings_path: &std::path::Path) -> HealthStatus {
    let now_ms = chrono::Local::now().timestamp_millis();
    let minute = 60_000;
    let listening = ReceiverStatus::Listening { port: 43177 };
    let transcripts = TranscriptsInfo {
        path: "~/.claude/projects".to_string(),
        exists: true,
    };
    let ingest =
        |ingested: u64, failures: u64, last_ms: i64, failure: Option<&str>| IngestStatsSnapshot {
            events_ingested: ingested,
            ingest_failures: failures,
            events_skipped: 7,
            last_event_ms: last_ms,
            last_failure: failure.map(str::to_string),
        };
    let stored = |count, last| {
        Ok(StoredEvents {
            count,
            last_event_ms: last,
        })
    };
    let display_path = settings_path.display().to_string();

    let (receiver, config, paused, ing, sto, tra) = match scenario {
        "fresh_machine" => (
            listening,
            ConfigState::Installed,
            false,
            ingest(0, 0, 0, None),
            stored(0, None),
            TranscriptsInfo {
                path: "~/.claude/projects".to_string(),
                exists: false,
            },
        ),
        "db_locked" => (
            listening,
            ConfigState::Installed,
            false,
            ingest(
                3,
                2,
                now_ms - 2 * minute,
                Some("could not store an api_request event: database is locked"),
            ),
            Err("The usage database could not be read (database is locked). Totals shown \
                 are since-launch only. If another copy of this app is running, quit it; \
                 otherwise check free disk space and relaunch."
                .to_string()),
            transcripts.clone(),
        ),
        "disk_full" => (
            listening,
            ConfigState::Installed,
            false,
            ingest(
                120,
                4,
                now_ms - 3 * minute,
                Some("could not store an api_request event: disk I/O error: database or disk is full"),
            ),
            stored(43_403, Some(now_ms - 3 * minute)),
            transcripts.clone(),
        ),
        "port_conflict" => (
            ReceiverStatus::PortInUse { port: 43177 },
            ConfigState::Installed,
            false,
            ingest(0, 0, 0, None),
            stored(43_403, Some(now_ms - 90 * minute)),
            transcripts.clone(),
        ),
        "receiver_failed" => (
            ReceiverStatus::Failed {
                message: "server stopped unexpectedly".to_string(),
            },
            ConfigState::Installed,
            false,
            ingest(12, 0, now_ms - 45 * minute, None),
            stored(43_403, Some(now_ms - 45 * minute)),
            transcripts.clone(),
        ),
        "paused" => (
            listening,
            ConfigState::Installed,
            true,
            ingest(12, 0, now_ms - 30 * minute, None),
            stored(43_403, Some(now_ms - 30 * minute)),
            transcripts.clone(),
        ),
        "config_unreadable" => {
            let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied")
                .into();
            (
                listening,
                ConfigState::Error {
                    message: describe_settings_error(&err, settings_path),
                },
                false,
                ingest(0, 0, 0, None),
                stored(43_403, Some(now_ms - 10 * minute)),
                transcripts.clone(),
            )
        }
        "config_conflicting" => {
            let map = serde_json::from_str::<serde_json::Value>(
                r#"{"env": {
                    "OTEL_LOGS_EXPORTER": "console",
                    "OTEL_EXPORTER_OTLP_ENDPOINT": "https://collector.example.com:4318"
                }}"#,
            )
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
            (
                listening,
                ConfigState::Conflicting {
                    installed: false,
                    conflicts: detect_conflicts(&map),
                },
                false,
                ingest(0, 0, 0, None),
                stored(0, None),
                transcripts.clone(),
            )
        }
        _ => (
            listening,
            ConfigState::Installed,
            false,
            ingest(42, 0, now_ms - 2 * minute, None),
            stored(43_403, Some(now_ms - 2 * minute)),
            transcripts.clone(),
        ),
    };

    compute_health(
        receiver,
        config,
        display_path,
        paused,
        ing,
        sto,
        tra,
        BackfillInfo::default(),
        now_ms,
    )
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
                HeaderValue::from_static("GET, POST, OPTIONS"),
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
