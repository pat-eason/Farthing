//! OTel log-event ingestion.
//!
//! Turns OTLP `http/json` logs payloads (already parsed to `serde_json::Value`
//! by the receiver) into `requests` rows. Only two event types are stored:
//!
//! - `claude_code.api_request` — the row-level source of truth: `cost_usd`,
//!   the four token counts, `model`, `query_source`, `session.id`, timestamp
//! - `claude_code.api_error` — stored with its error metadata for the
//!   error counts surfaced in the desktop UI (PRD FR-1)
//!
//! Everything else Claude Code emits (`hook_execution_*`,
//!   `mcp_server_connection`, `user_prompt`, …) is silently skipped.
//!
//! # Version tolerance
//!
//! The event schema is undocumented and may drift between Claude Code
//! releases, so parsing is deliberately permissive:
//!
//! - unknown event names and unknown attributes are ignored without error
//! - `intValue` is accepted as both a JSON number (`41`) and a string
//!   (`"248"`) — the real exporter emits **both within one batch**
//! - the event name is read from the `event.name` attribute (observed:
//!   unprefixed `"api_request"`) with `body.stringValue` (observed: prefixed
//!   `"claude_code.api_request"`) as fallback; the `claude_code.` prefix is
//!   accepted in either spot
//! - only `session.id` and a timestamp are required; a record missing them
//!   increments the ingest-failure counter (surfaced by the health view,
//!   task 2.5) instead of erroring the export
//!
//! See `tests/fixtures/README.md` for the captured payload these rules were
//! derived from.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::db::Db;

/// Counters for ingest health, shared between the receiver task and the
/// `ingest_stats` command. Monotonic except `last_event_ms`.
#[derive(Debug, Default)]
pub struct IngestStats {
    /// Rows successfully written to `requests`.
    events_ingested: AtomicU64,
    /// Records that looked like ours but could not be stored (missing
    /// required fields, or a database write error).
    ingest_failures: AtomicU64,
    /// Log records skipped because the event name is not one we store.
    events_skipped: AtomicU64,
    /// Wall-clock ms of the most recent successful ingest (0 = never).
    last_event_ms: AtomicI64,
}

/// Point-in-time copy of [`IngestStats`] for the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestStatsSnapshot {
    pub events_ingested: u64,
    pub ingest_failures: u64,
    pub events_skipped: u64,
    /// 0 when no event has ever been ingested.
    pub last_event_ms: i64,
}

impl IngestStats {
    pub fn snapshot(&self) -> IngestStatsSnapshot {
        IngestStatsSnapshot {
            events_ingested: self.events_ingested.load(Ordering::Relaxed),
            ingest_failures: self.ingest_failures.load(Ordering::Relaxed),
            events_skipped: self.events_skipped.load(Ordering::Relaxed),
            last_event_ms: self.last_event_ms.load(Ordering::Relaxed),
        }
    }

    fn record_success(&self) {
        self.events_ingested.fetch_add(1, Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.last_event_ms.store(now_ms, Ordering::Relaxed);
    }

    fn record_failure(&self) {
        self.ingest_failures.fetch_add(1, Ordering::Relaxed);
    }

    fn record_skip(&self) {
        self.events_skipped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Everything the `/v1/logs` handler needs: the shared database handle and
/// the shared counters. Cloned into the axum router as its state.
#[derive(Clone)]
pub struct IngestState {
    pub db: Arc<Mutex<Db>>,
    pub stats: Arc<IngestStats>,
}

impl IngestState {
    pub fn new(db: Arc<Mutex<Db>>) -> Self {
        Self {
            db,
            stats: Arc::new(IngestStats::default()),
        }
    }
}

/// Query ingest counters from the frontend (health view, task 2.5).
#[tauri::command]
pub fn ingest_stats(state: tauri::State<'_, IngestState>) -> IngestStatsSnapshot {
    state.stats.snapshot()
}

/// A parsed, storable event. Field names mirror the `requests` columns.
#[derive(Debug, PartialEq)]
struct RequestRow {
    request_id: Option<String>,
    session_id: String,
    timestamp_ms: i64,
    model: Option<String>,
    query_source: Option<String>,
    cost_usd: Option<f64>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    event_type: &'static str,
    error: Option<String>,
    duration_ms: Option<i64>,
}

/// Walk an OTLP logs payload and ingest every storable record. Never fails:
/// per-record problems are tallied in `stats`, unknown shapes are skipped.
pub fn ingest_logs(state: &IngestState, payload: &Value) {
    for record in log_records(payload) {
        ingest_record(state, record);
    }
}

/// Flatten `resourceLogs[].scopeLogs[].logRecords[]`, tolerating any level
/// being absent or of the wrong type.
fn log_records(payload: &Value) -> impl Iterator<Item = &Value> {
    as_array(payload.get("resourceLogs"))
        .iter()
        .flat_map(|resource| as_array(resource.get("scopeLogs")))
        .flat_map(|scope| as_array(scope.get("logRecords")))
}

fn as_array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn ingest_record(state: &IngestState, record: &Value) {
    let Some(name) = event_name(record) else {
        // No event name at all: not a Claude Code event record; skip.
        state.stats.record_skip();
        return;
    };
    let event_type = match name {
        "api_request" => "api_request",
        "api_error" => "api_error",
        _ => {
            state.stats.record_skip();
            return;
        }
    };
    match parse_event(record, event_type) {
        Some(row) => match insert_row(state, &row) {
            Ok(()) => state.stats.record_success(),
            Err(err) => {
                state.stats.record_failure();
                eprintln!("ingest: failed to store {event_type} row: {err}");
            }
        },
        None => state.stats.record_failure(),
    }
}

/// Resolve the event name, normalized without the `claude_code.` prefix.
/// Primary source is the `event.name` attribute (observed unprefixed);
/// fallback is `body.stringValue` (observed prefixed).
fn event_name(record: &Value) -> Option<&str> {
    attr_str(record, "event.name")
        .or_else(|| {
            record
                .get("body")
                .and_then(|body| body.get("stringValue"))
                .and_then(Value::as_str)
        })
        .map(|name| name.strip_prefix("claude_code.").unwrap_or(name))
}

/// Parse one record into a row. Returns `None` when a required field
/// (`session.id`, timestamp) is missing — counted as an ingest failure.
fn parse_event(record: &Value, event_type: &'static str) -> Option<RequestRow> {
    let session_id = attr_str(record, "session.id")?.to_owned();
    let timestamp_ms = record_timestamp_ms(record)?;

    Some(RequestRow {
        request_id: attr_str(record, "request_id").map(str::to_owned),
        session_id,
        timestamp_ms,
        model: attr_str(record, "model").map(str::to_owned),
        query_source: attr_str(record, "query_source").map(str::to_owned),
        cost_usd: attr_f64(record, "cost_usd"),
        input_tokens: attr_i64(record, "input_tokens").unwrap_or(0),
        output_tokens: attr_i64(record, "output_tokens").unwrap_or(0),
        cache_read_tokens: attr_i64(record, "cache_read_tokens").unwrap_or(0),
        cache_creation_tokens: attr_i64(record, "cache_creation_tokens").unwrap_or(0),
        event_type,
        error: attr_str(record, "error").map(str::to_owned),
        duration_ms: attr_i64(record, "duration_ms"),
    })
}

/// Event time in unix ms from `timeUnixNano` (fallback
/// `observedTimeUnixNano`). Both arrive as strings of nanoseconds; numbers
/// are tolerated too.
fn record_timestamp_ms(record: &Value) -> Option<i64> {
    ["timeUnixNano", "observedTimeUnixNano"]
        .iter()
        .find_map(|key| value_i64(record.get(*key)?))
        .map(|nanos| nanos / 1_000_000)
}

/// Find an attribute by key in the record's `attributes` array and return
/// its OTLP `AnyValue` object.
fn attr_value<'a>(record: &'a Value, key: &str) -> Option<&'a Value> {
    as_array(record.get("attributes"))
        .iter()
        .find(|attr| attr.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")
}

fn attr_str<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    attr_value(record, key)?
        .get("stringValue")
        .and_then(Value::as_str)
}

/// Integer attribute: `intValue` as JSON number or string (the real
/// exporter emits both), with `doubleValue` as a last resort.
fn attr_i64(record: &Value, key: &str) -> Option<i64> {
    let value = attr_value(record, key)?;
    value.get("intValue").and_then(value_i64).or_else(|| {
        value
            .get("doubleValue")
            .and_then(Value::as_f64)
            .map(|f| f as i64)
    })
}

/// Float attribute: `doubleValue` (number or string) or `intValue`.
fn attr_f64(record: &Value, key: &str) -> Option<f64> {
    let value = attr_value(record, key)?;
    value
        .get("doubleValue")
        .and_then(value_f64)
        .or_else(|| value.get("intValue").and_then(value_i64).map(|i| i as f64))
}

/// An i64 encoded as either a JSON number or a string.
fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// An f64 encoded as either a JSON number or a string.
fn value_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn insert_row(state: &IngestState, row: &RequestRow) -> Result<(), rusqlite::Error> {
    let db = state.db.lock().expect("db mutex poisoned");
    db.conn()
        .execute(
            "INSERT INTO requests (
                request_id, session_id, timestamp_ms, model, query_source,
                cost_usd, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, event_type, error, duration_ms, source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'otel')",
            rusqlite::params![
                row.request_id,
                row.session_id,
                row.timestamp_ms,
                row.model,
                row.query_source,
                row.cost_usd,
                row.input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_creation_tokens,
                row.event_type,
                row.error,
                row.duration_ms,
            ],
        )
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real Claude Code v2.1.173 session (sanitized). One
    /// `api_request` among 11 hook/MCP records — see fixtures/README.md.
    const REAL_API_REQUEST_BATCH: &str =
        include_str!("../tests/fixtures/otlp_logs_api_request.json");
    /// Reconstructed `api_error` on the captured envelope.
    const API_ERROR_BATCH: &str = include_str!("../tests/fixtures/otlp_logs_api_error.json");

    fn test_state() -> (IngestState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (IngestState::new(Arc::new(Mutex::new(db))), dir)
    }

    fn ingest_str(state: &IngestState, payload: &str) {
        ingest_logs(state, &serde_json::from_str(payload).unwrap());
    }

    fn row_count(state: &IngestState) -> i64 {
        let db = state.db.lock().unwrap();
        db.conn()
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn real_capture_ingests_exactly_the_api_request() {
        let (state, _dir) = test_state();
        ingest_str(&state, REAL_API_REQUEST_BATCH);

        assert_eq!(row_count(&state), 1);
        let stats = state.stats.snapshot();
        assert_eq!(stats.events_ingested, 1);
        assert_eq!(stats.ingest_failures, 0);
        assert_eq!(stats.events_skipped, 11); // hook_* + mcp_server_connection
        assert!(stats.last_event_ms > 0);

        let db = state.db.lock().unwrap();
        db.conn()
            .query_row("SELECT * FROM requests", [], |row| {
                assert_eq!(
                    row.get::<_, String>("request_id")?,
                    "req_011CbwuYCfawVQtYFZaU7Kgi"
                );
                assert_eq!(
                    row.get::<_, String>("session_id")?,
                    "c2399881-2a19-4df5-9649-7a67248d135c"
                );
                // timeUnixNano 1781200718939000000 → ms
                assert_eq!(row.get::<_, i64>("timestamp_ms")?, 1_781_200_718_939);
                assert_eq!(row.get::<_, String>("model")?, "claude-haiku-4-5-20251001");
                assert_eq!(row.get::<_, String>("query_source")?, "sdk");
                assert!((row.get::<_, f64>("cost_usd")? - 0.0046586).abs() < 1e-9);
                assert_eq!(row.get::<_, i64>("input_tokens")?, 10);
                assert_eq!(row.get::<_, i64>("output_tokens")?, 41);
                assert_eq!(row.get::<_, i64>("cache_read_tokens")?, 44_436);
                assert_eq!(row.get::<_, i64>("cache_creation_tokens")?, 0);
                assert_eq!(row.get::<_, String>("event_type")?, "api_request");
                assert!(row.get::<_, Option<String>>("error")?.is_none());
                assert_eq!(row.get::<_, i64>("duration_ms")?, 1648);
                assert_eq!(row.get::<_, String>("source")?, "otel");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn api_error_is_stored_with_error_metadata() {
        let (state, _dir) = test_state();
        ingest_str(&state, API_ERROR_BATCH);

        assert_eq!(state.stats.snapshot().events_ingested, 1);
        let db = state.db.lock().unwrap();
        db.conn()
            .query_row(
                "SELECT * FROM requests WHERE event_type = 'api_error'",
                [],
                |row| {
                    assert!(row.get::<_, String>("error")?.contains("overloaded_error"));
                    assert_eq!(row.get::<_, String>("model")?, "claude-haiku-4-5-20251001");
                    // duration_ms encoded as intValue *string* in the fixture
                    assert_eq!(row.get::<_, i64>("duration_ms")?, 2105);
                    assert_eq!(
                        row.get::<_, String>("session_id")?,
                        "c2399881-2a19-4df5-9649-7a67248d135c"
                    );
                    Ok(())
                },
            )
            .unwrap();
    }

    #[test]
    fn unknown_events_and_empty_payloads_are_ignored_without_error() {
        let (state, _dir) = test_state();
        // A no-API-request session really exports an empty object.
        ingest_str(&state, "{}");
        // Unknown future event name with unknown attributes.
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "timeUnixNano": "1781200718939000000",
                "attributes": [
                    {"key": "event.name", "value": {"stringValue": "quantum_compaction"}},
                    {"key": "novel.attribute", "value": {"kvlistValue": {"values": []}}}
                ]
            }]}]}]}"#,
        );
        // Structurally weird levels: wrong types everywhere.
        ingest_str(&state, r#"{"resourceLogs": "nope"}"#);
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [42, {}]}]}]}"#,
        );

        assert_eq!(row_count(&state), 0);
        let stats = state.stats.snapshot();
        assert_eq!(stats.events_ingested, 0);
        assert_eq!(stats.ingest_failures, 0);
        assert!(stats.events_skipped >= 1);
    }

    #[test]
    fn unknown_attributes_on_api_request_are_ignored() {
        let (state, _dir) = test_state();
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "timeUnixNano": "1781200718939000000",
                "attributes": [
                    {"key": "event.name", "value": {"stringValue": "api_request"}},
                    {"key": "session.id", "value": {"stringValue": "sess-1"}},
                    {"key": "input_tokens", "value": {"intValue": "7"}},
                    {"key": "from_the_future", "value": {"boolValue": true}},
                    {"key": "model", "value": {"intValue": 9000}}
                ]
            }]}]}]}"#,
        );

        assert_eq!(row_count(&state), 1);
        assert_eq!(state.stats.snapshot().ingest_failures, 0);
        let db = state.db.lock().unwrap();
        db.conn()
            .query_row("SELECT * FROM requests", [], |row| {
                // intValue-as-string parsed; wrong-typed model treated as absent.
                assert_eq!(row.get::<_, i64>("input_tokens")?, 7);
                assert!(row.get::<_, Option<String>>("model")?.is_none());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn missing_required_fields_increment_failure_counter() {
        let (state, _dir) = test_state();
        // api_request without session.id
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "timeUnixNano": "1781200718939000000",
                "attributes": [{"key": "event.name", "value": {"stringValue": "api_request"}}]
            }]}]}]}"#,
        );
        // api_error without any timestamp
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "attributes": [
                    {"key": "event.name", "value": {"stringValue": "api_error"}},
                    {"key": "session.id", "value": {"stringValue": "sess-1"}}
                ]
            }]}]}]}"#,
        );

        assert_eq!(row_count(&state), 0);
        let stats = state.stats.snapshot();
        assert_eq!(stats.ingest_failures, 2);
        assert_eq!(stats.events_ingested, 0);
    }

    #[test]
    fn prefixed_body_name_is_accepted_without_event_name_attribute() {
        let (state, _dir) = test_state();
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "timeUnixNano": "1781200718939000000",
                "body": {"stringValue": "claude_code.api_request"},
                "attributes": [{"key": "session.id", "value": {"stringValue": "sess-1"}}]
            }]}]}]}"#,
        );
        assert_eq!(row_count(&state), 1);
    }

    #[test]
    fn observed_time_is_timestamp_fallback() {
        let (state, _dir) = test_state();
        ingest_str(
            &state,
            r#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [{
                "observedTimeUnixNano": 1781200718939000000,
                "attributes": [
                    {"key": "event.name", "value": {"stringValue": "api_request"}},
                    {"key": "session.id", "value": {"stringValue": "sess-1"}}
                ]
            }]}]}]}"#,
        );
        assert_eq!(row_count(&state), 1);
        let db = state.db.lock().unwrap();
        let ts: i64 = db
            .conn()
            .query_row("SELECT timestamp_ms FROM requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ts, 1_781_200_718_939);
    }

    #[test]
    fn stats_snapshot_serializes_for_frontend() {
        let stats = IngestStats::default();
        stats.record_failure();
        let json = serde_json::to_value(stats.snapshot()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "events_ingested": 0,
                "ingest_failures": 1,
                "events_skipped": 0,
                "last_event_ms": 0,
            })
        );
    }
}
