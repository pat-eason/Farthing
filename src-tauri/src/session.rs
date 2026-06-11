//! Session mapping endpoint.
//!
//! `POST /session` receives the Claude Code `SessionStart` hook's stdin JSON
//! (the installer wires the hook to `curl --max-time 2 --silent` against
//! this endpoint, task 2.1) and upserts the `session_id → cwd` mapping into
//! `sessions`. OTel events carry `session.id` but not `cwd` (verified gap,
//! PRD FR-3), so this mapping is what joins live `requests` rows to a
//! project at query time.
//!
//! # Hook contract
//!
//! SessionStart stdin JSON (documented Claude Code hook surface):
//!
//! ```json
//! {
//!   "session_id": "c2399881-...",
//!   "transcript_path": "~/.claude/projects/<encoded-cwd>/<id>.jsonl",
//!   "cwd": "/Users/me/Projects/foo",
//!   "hook_event_name": "SessionStart",
//!   "source": "startup"
//! }
//! ```
//!
//! Parsing is version tolerant: only a non-empty `session_id` is required;
//! `cwd` is stored when present and unknown fields are ignored. The hook's
//! `source` field (`startup`/`resume`/`clear`/`compact`) is *not* the
//! `sessions.source` column — that records the mapping's provenance
//! (`'hook'` here, `'backfill'` for transcript recovery, epic 3).
//!
//! # Latency
//!
//! The hook must never slow Claude Code down (PRD NFR: fail-silent curl,
//! 2s timeout), so the handler budgets [`WRITE_WAIT_BUDGET`] for the
//! database write. Under contention (busy_timeout is 5s) it responds `202`
//! within the budget and lets the spawned write finish in the background;
//! the hook ignores the body either way.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::{json, Value};

use crate::db::Db;
use crate::ingest::IngestState;

/// How long the handler waits for the database write before responding 202
/// and letting the write complete in the background. Well under the 100ms
/// response budget (plan 1.5) and the hook curl's 2s timeout.
const WRITE_WAIT_BUDGET: Duration = Duration::from_millis(50);

/// The fields kept from a SessionStart hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMapping {
    session_id: String,
    cwd: Option<String>,
}

/// `POST /session`: upsert the SessionStart hook's `session_id → cwd`
/// mapping. 200 = written, 202 = accepted (write still in flight after the
/// wait budget), 400 = unusable payload, 500 = database write failed.
pub async fn post_session(State(ingest): State<IngestState>, body: Bytes) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => return bad_request(&format!("invalid JSON payload: {err}")),
    };
    let Some(mapping) = parse_hook_payload(&payload) else {
        return bad_request("missing or empty session_id");
    };

    let db = Arc::clone(&ingest.db);
    let write = tokio::task::spawn_blocking(move || upsert_session(&db, &mapping, now_ms()));

    match tokio::time::timeout(WRITE_WAIT_BUDGET, write).await {
        Ok(Ok(Ok(()))) => (StatusCode::OK, Json(json!({}))).into_response(),
        Ok(Ok(Err(err))) => {
            eprintln!("session: failed to upsert mapping: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "message": "failed to store session mapping" })),
            )
                .into_response()
        }
        Ok(Err(join_err)) => {
            eprintln!("session: write task panicked: {join_err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "message": "failed to store session mapping" })),
            )
                .into_response()
        }
        // Budget exceeded: the blocking task keeps running and will land the
        // write once the database frees up; respond now so the hook returns.
        Err(_elapsed) => (StatusCode::ACCEPTED, Json(json!({}))).into_response(),
    }
}

/// Extract the mapping from the hook payload. `None` when `session_id` is
/// missing, empty, or not a string; everything else is optional.
fn parse_hook_payload(payload: &Value) -> Option<SessionMapping> {
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(SessionMapping { session_id, cwd })
}

/// Insert or refresh the mapping. Idempotent for repeat POSTs of the same
/// session: `first_seen_ms` is preserved, `last_seen_ms` advances, and a
/// payload without `cwd` never clobbers a previously stored one. A hook
/// sighting upgrades `source` to `'hook'` (live data beats backfill).
fn upsert_session(
    db: &Mutex<Db>,
    mapping: &SessionMapping,
    now_ms: i64,
) -> Result<(), rusqlite::Error> {
    let db = db.lock().expect("db mutex poisoned");
    db.conn()
        .execute(
            "INSERT INTO sessions (session_id, cwd, first_seen_ms, last_seen_ms, source)
             VALUES (?1, ?2, ?3, ?3, 'hook')
             ON CONFLICT (session_id) DO UPDATE SET
                 cwd = COALESCE(excluded.cwd, cwd),
                 last_seen_ms = excluded.last_seen_ms,
                 source = 'hook'",
            rusqlite::params![mapping.session_id, mapping.cwd, now_ms],
        )
        .map(|_| ())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 400 with a small JSON body (the hook discards it; humans debugging with
/// curl get a reason).
fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;

    /// Real SessionStart hook stdin shape (documented contract).
    const HOOK_PAYLOAD: &str = r#"{
        "session_id": "c2399881-2a19-4df5-9649-7a67248d135c",
        "transcript_path": "~/.claude/projects/-Users-me-Projects-foo/c2399881.jsonl",
        "cwd": "/Users/me/Projects/foo",
        "hook_event_name": "SessionStart",
        "source": "startup"
    }"#;

    fn test_state() -> (IngestState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (IngestState::new(Arc::new(Mutex::new(db))), dir)
    }

    async fn post(state: &IngestState, body: &str) -> StatusCode {
        post_session(State(state.clone()), Bytes::from(body.to_owned()))
            .await
            .status()
    }

    /// (cwd, first_seen_ms, last_seen_ms, source) for a session, or None.
    fn session_row(
        state: &IngestState,
        session_id: &str,
    ) -> Option<(Option<String>, i64, i64, String)> {
        let db = state.db.lock().unwrap();
        db.conn()
            .query_row(
                "SELECT cwd, first_seen_ms, last_seen_ms, source
                 FROM sessions WHERE session_id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .ok()
    }

    fn session_count(state: &IngestState) -> i64 {
        let db = state.db.lock().unwrap();
        db.conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn hook_payload_upserts_mapping_with_hook_source() {
        let (state, _dir) = test_state();
        assert_eq!(post(&state, HOOK_PAYLOAD).await, StatusCode::OK);

        let (cwd, first_seen, last_seen, source) =
            session_row(&state, "c2399881-2a19-4df5-9649-7a67248d135c").unwrap();
        assert_eq!(cwd.as_deref(), Some("/Users/me/Projects/foo"));
        assert!(first_seen > 0);
        assert_eq!(last_seen, first_seen);
        assert_eq!(source, "hook");
    }

    #[tokio::test]
    async fn repeat_posts_are_idempotent() {
        let (state, _dir) = test_state();
        assert_eq!(post(&state, HOOK_PAYLOAD).await, StatusCode::OK);
        let (_, first_seen_before, _, _) =
            session_row(&state, "c2399881-2a19-4df5-9649-7a67248d135c").unwrap();

        // Same session, e.g. a `resume` SessionStart.
        assert_eq!(post(&state, HOOK_PAYLOAD).await, StatusCode::OK);
        assert_eq!(post(&state, HOOK_PAYLOAD).await, StatusCode::OK);

        assert_eq!(session_count(&state), 1);
        let (cwd, first_seen, last_seen, _) =
            session_row(&state, "c2399881-2a19-4df5-9649-7a67248d135c").unwrap();
        assert_eq!(first_seen, first_seen_before, "first_seen must not move");
        assert!(last_seen >= first_seen);
        assert_eq!(cwd.as_deref(), Some("/Users/me/Projects/foo"));
    }

    #[tokio::test]
    async fn missing_cwd_never_clobbers_a_stored_one() {
        let (state, _dir) = test_state();
        assert_eq!(post(&state, HOOK_PAYLOAD).await, StatusCode::OK);
        assert_eq!(
            post(
                &state,
                r#"{"session_id": "c2399881-2a19-4df5-9649-7a67248d135c"}"#
            )
            .await,
            StatusCode::OK
        );

        let (cwd, ..) = session_row(&state, "c2399881-2a19-4df5-9649-7a67248d135c").unwrap();
        assert_eq!(cwd.as_deref(), Some("/Users/me/Projects/foo"));
    }

    #[tokio::test]
    async fn unusable_payloads_return_400_and_write_nothing() {
        let (state, _dir) = test_state();
        for body in [
            "{not json",
            "{}",
            r#"{"session_id": ""}"#,
            r#"{"session_id": 42, "cwd": "/tmp"}"#,
            r#"{"cwd": "/tmp"}"#,
        ] {
            assert_eq!(post(&state, body).await, StatusCode::BAD_REQUEST, "{body}");
        }
        assert_eq!(session_count(&state), 0);
    }

    #[tokio::test]
    async fn unknown_fields_are_tolerated() {
        let (state, _dir) = test_state();
        let status = post(
            &state,
            r#"{
                "session_id": "sess-future",
                "cwd": "/tmp/p",
                "hook_event_name": "SessionStart",
                "source": "compact",
                "permission_mode": "from-the-future",
                "extra": {"deeply": ["nested", 1]}
            }"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(session_row(&state, "sess-future").is_some());
    }

    /// Plan 1.5: responds within 100ms even when the database is held by
    /// another writer; the upsert lands once contention clears.
    #[tokio::test(flavor = "multi_thread")]
    async fn responds_under_100ms_during_db_contention_then_writes() {
        let (state, _dir) = test_state();

        // Hold the db mutex on another thread to simulate a long writer
        // (busy_timeout alone is 5s; the mutex models any contention).
        let contended = Arc::clone(&state.db);
        let hold = std::thread::spawn(move || {
            let guard = contended.lock().unwrap();
            std::thread::sleep(Duration::from_millis(400));
            drop(guard);
        });

        let started = Instant::now();
        let status = post(&state, HOOK_PAYLOAD).await;
        let elapsed = started.elapsed();

        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            elapsed < Duration::from_millis(100),
            "response took {elapsed:?}"
        );

        // The write completes in the background once the lock frees.
        hold.join().unwrap();
        for _ in 0..100 {
            if session_row(&state, "c2399881-2a19-4df5-9649-7a67248d135c").is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("background write never landed");
    }
}
