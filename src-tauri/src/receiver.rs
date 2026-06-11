//! Embedded OTLP receiver.
//!
//! A localhost-only axum server on fixed port 43177 that accepts OTLP
//! `http/json` exports from Claude Code:
//!
//! - `POST /v1/logs` — the live event pipeline (parsing into `requests` rows
//!   lands in task 1.4; for now well-formed payloads are accepted and
//!   discarded)
//! - `POST /v1/metrics` — accepted and discarded by design; aggregations are
//!   derived in SQL from log events (PRD FR-1)
//!
//! The port is never auto-rebound: `settings.json` holds the literal
//! endpoint, so a different port would silently break export. A port
//! conflict at startup is recorded in [`ReceiverStatus`] and surfaced to the
//! frontend via the `receiver_status` command (consumed by the health view,
//! task 2.5).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde::Serialize;
use serde_json::json;

/// Fixed OTLP ingest port. Non-standard on purpose: avoids collision with a
/// user-run collector on the standard OTLP ports 4317/4318.
pub const OTLP_PORT: u16 = 43177;

/// Lifecycle state of the receiver, queryable from the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReceiverStatus {
    /// Bind not attempted/completed yet.
    Starting,
    /// Bound and serving.
    Listening { port: u16 },
    /// Another process holds the port. We never auto-rebind; remediation is
    /// up to the user (health view, task 2.5).
    PortInUse { port: u16 },
    /// Bind or serve failed for any other reason.
    Failed { message: String },
}

/// Shared, mutable receiver status.
pub type SharedStatus = Arc<Mutex<ReceiverStatus>>;

/// Tauri-managed wrapper around the receiver status.
pub struct ReceiverState(pub SharedStatus);

/// Create the status cell in its initial state.
pub fn new_status() -> SharedStatus {
    Arc::new(Mutex::new(ReceiverStatus::Starting))
}

/// Query the receiver status from the frontend.
#[tauri::command]
pub fn receiver_status(state: tauri::State<'_, ReceiverState>) -> ReceiverStatus {
    state
        .0
        .lock()
        .expect("receiver status mutex poisoned")
        .clone()
}

/// Bind `127.0.0.1:43177` and serve until the app exits, recording every
/// state transition in `status`. Intended to be spawned on the Tauri async
/// runtime.
pub async fn run(status: SharedStatus) {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, OTLP_PORT));
    serve_on(addr, status).await;
}

/// Bind `addr` (loopback expected) and serve. Split from [`run`] so tests
/// can use an ephemeral port. Returns when the server stops (bind failure
/// or fatal serve error).
pub async fn serve_on(addr: SocketAddr, status: SharedStatus) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            set_status(&status, ReceiverStatus::PortInUse { port: addr.port() });
            return;
        }
        Err(err) => {
            set_status(
                &status,
                ReceiverStatus::Failed {
                    message: format!("failed to bind {addr}: {err}"),
                },
            );
            return;
        }
    };

    // With an ephemeral port (tests) the real port is only known post-bind.
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(addr.port());
    set_status(&status, ReceiverStatus::Listening { port });

    if let Err(err) = axum::serve(listener, router()).await {
        set_status(
            &status,
            ReceiverStatus::Failed {
                message: format!("server stopped: {err}"),
            },
        );
    }
}

fn set_status(status: &SharedStatus, next: ReceiverStatus) {
    *status.lock().expect("receiver status mutex poisoned") = next;
}

/// OTLP `http/json` routes. Stateless for now; task 1.4 threads the DB
/// handle through `Router::with_state` to turn `/v1/logs` into real ingest.
pub fn router() -> Router {
    Router::new()
        .route("/v1/logs", post(post_logs))
        .route("/v1/metrics", post(post_metrics))
}

/// `POST /v1/logs`: validate the payload is JSON, then (for now) discard.
/// Task 1.4 replaces the discard with `claude_code.api_request` ingestion.
async fn post_logs(body: Bytes) -> Response {
    match parse_otlp_json(&body) {
        Ok(_payload) => export_success(),
        Err(err) => invalid_json(&err),
    }
}

/// `POST /v1/metrics`: accept and discard. The installer never enables the
/// metrics exporter, but a 200 here keeps any client that does happy.
async fn post_metrics(body: Bytes) -> Response {
    match parse_otlp_json(&body) {
        Ok(_payload) => export_success(),
        Err(err) => invalid_json(&err),
    }
}

/// Parse the request body as JSON. Content negotiation is deliberately
/// lenient (no content-type enforcement): version tolerance matters more
/// than strictness for an undocumented exporter surface.
fn parse_otlp_json(body: &Bytes) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_slice(body)
}

/// 400 with an OTLP/HTTP-style Status body (code 3 = gRPC INVALID_ARGUMENT).
fn invalid_json(err: &serde_json::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "code": 3,
            "message": format!("invalid JSON payload: {err}"),
        })),
    )
        .into_response()
}

/// OTLP/HTTP full-success export response (empty partialSuccess).
fn export_success() -> Response {
    (StatusCode::OK, Json(json!({}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Realistic OTLP `http/json` logs payload (shape per opentelemetry-proto).
    const OTLP_LOGS_BODY: &str = r#"{
        "resourceLogs": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "claude-code"}}]},
            "scopeLogs": [{
                "scope": {"name": "com.anthropic.claude_code.events"},
                "logRecords": [{
                    "timeUnixNano": "1718100000000000000",
                    "body": {"stringValue": "claude_code.api_request"},
                    "attributes": [
                        {"key": "event.name", "value": {"stringValue": "claude_code.api_request"}},
                        {"key": "session.id", "value": {"stringValue": "sess-1"}}
                    ]
                }]
            }]
        }]
    }"#;

    const OTLP_METRICS_BODY: &str = r#"{
        "resourceMetrics": [{
            "resource": {"attributes": []},
            "scopeMetrics": [{
                "metrics": [{"name": "claude_code.token.usage", "sum": {"dataPoints": []}}]
            }]
        }]
    }"#;

    /// Spawn the receiver on an ephemeral loopback port and wait for it to
    /// report Listening. Returns the bound address and the status cell.
    async fn start_test_receiver() -> (SocketAddr, SharedStatus) {
        let status = new_status();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        tokio::spawn(serve_on(addr, Arc::clone(&status)));

        for _ in 0..100 {
            let snapshot = status.lock().unwrap().clone();
            if let ReceiverStatus::Listening { port } = snapshot {
                return (SocketAddr::from((Ipv4Addr::LOCALHOST, port)), status);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("receiver never reached Listening state");
    }

    /// Minimal raw HTTP/1.1 client: returns the response status code.
    async fn http_post(addr: SocketAddr, path: &str, body: &str) -> u16 {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
            .strip_prefix("HTTP/1.1 ")
            .and_then(|rest| rest.get(..3))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("unparseable response: {response}"))
    }

    #[test]
    fn fixed_port_is_43177() {
        assert_eq!(OTLP_PORT, 43177);
    }

    #[tokio::test]
    async fn wellformed_logs_and_metrics_return_200() {
        let (addr, _status) = start_test_receiver().await;
        assert_eq!(http_post(addr, "/v1/logs", OTLP_LOGS_BODY).await, 200);
        assert_eq!(http_post(addr, "/v1/metrics", OTLP_METRICS_BODY).await, 200);
    }

    #[tokio::test]
    async fn malformed_json_returns_400_without_killing_server() {
        let (addr, status) = start_test_receiver().await;
        assert_eq!(http_post(addr, "/v1/logs", "{not json").await, 400);
        assert_eq!(http_post(addr, "/v1/metrics", "").await, 400);

        // Server survived: still listening and still serving good payloads.
        assert!(matches!(
            *status.lock().unwrap(),
            ReceiverStatus::Listening { .. }
        ));
        assert_eq!(http_post(addr, "/v1/logs", OTLP_LOGS_BODY).await, 200);
    }

    #[tokio::test]
    async fn unknown_route_is_404_not_a_crash() {
        let (addr, _status) = start_test_receiver().await;
        assert_eq!(http_post(addr, "/v1/traces", "{}").await, 404);
    }

    #[tokio::test]
    async fn binds_loopback_only() {
        let (addr, _status) = start_test_receiver().await;
        assert!(addr.ip().is_loopback());

        // If this machine has a non-loopback address, a connection to it on
        // the receiver's port must fail (nothing is bound there).
        if let Some(external_ip) = non_loopback_local_ip() {
            let external = SocketAddr::from((external_ip, addr.port()));
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(external),
            )
            .await;
            // Refused (Ok(Err)) or timed out (Err) is correct behavior.
            if let Ok(Ok(_)) = result {
                panic!("non-loopback connection to {external} unexpectedly succeeded");
            }
        }
    }

    /// Discover this host's primary non-loopback IPv4 address via a UDP
    /// "connect" (no packets sent). None if the host has no route.
    fn non_loopback_local_ip() -> Option<std::net::IpAddr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("198.51.100.1:9").ok()?; // TEST-NET-2, never routed
        let ip = socket.local_addr().ok()?.ip();
        (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
    }

    #[tokio::test]
    async fn port_in_use_is_detected_and_not_rebound() {
        // Occupy an ephemeral port, then point the receiver at it.
        let blocker = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let taken = blocker.local_addr().unwrap();

        let status = new_status();
        serve_on(taken, Arc::clone(&status)).await; // returns immediately on bind failure

        assert_eq!(
            *status.lock().unwrap(),
            ReceiverStatus::PortInUse { port: taken.port() }
        );
    }

    #[test]
    fn status_serializes_for_frontend() {
        let listening =
            serde_json::to_value(ReceiverStatus::Listening { port: OTLP_PORT }).unwrap();
        assert_eq!(
            listening,
            serde_json::json!({"state": "listening", "port": 43177})
        );
        let in_use = serde_json::to_value(ReceiverStatus::PortInUse { port: OTLP_PORT }).unwrap();
        assert_eq!(
            in_use,
            serde_json::json!({"state": "port_in_use", "port": 43177})
        );
    }
}
