//! Transcript backfill engine (task 3.4, PRD FR-5).
//!
//! Recovers usage the live OTel pipeline missed (app not yet installed, app
//! down, capture paused) from the transcripts Claude Code already writes
//! under `~/.claude/projects/`. One [`run_pass`] does both jobs:
//!
//! - **first run**: every `*.jsonl` under the projects root (including
//!   `<session-id>/subagents/**.jsonl` sidechains) is parsed from byte 0 —
//!   all available history, which Claude Code itself bounds at roughly 30
//!   days (`cleanupPeriodDays` default)
//! - **incremental runs** (every app start, and 3.5's "Backfill now"): each
//!   file resumes at its stored `ingest_state.byte_offset`, so an unchanged
//!   corpus costs one `stat` per file and re-runs ingest zero new rows
//!
//! # Dedup & precedence (spike 3.1, `docs/notes/dedup-key.md`)
//!
//! Rows are keyed by exact `request_id` (partial unique index, schema v2).
//! Backfill inserts `source='backfill'` rows with cost computed from the
//! pricing table (task 3.3; unknown model → `cost_usd = NULL`, the
//! tokens-only flag). On conflict with an existing row — live otel or a
//! previous backfill pass — the existing row wins and backfill only fills
//! the transcript-exclusive 5m/1h cache-creation split when it is missing.
//! The mirror-image race (backfill row first, live export seconds later) is
//! handled on the otel side: `ingest.rs` takes the row over.
//!
//! # Session self-heal
//!
//! Every transcript line carries `sessionId` + `cwd`, so backfill also
//! repairs the `sessions` table: sessions whose SessionStart hook POST never
//! arrived are created with `source='backfill'`, and existing hook rows
//! missing `cwd` get it filled (hook data is never overwritten).
//!
//! # Offsets & truncation
//!
//! `transcript::TranscriptParse::bytes_consumed` excludes a trailing
//! unterminated line, so a file Claude Code is mid-writing is resumed
//! cleanly next pass. A file *shorter* than its stored offset was replaced
//! or truncated; its offset resets to 0 and the unique index makes the
//! re-read idempotent.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::Db;
use crate::pricing::{PricingState, UsageTokens};
use crate::transcript::{self, AssistantUsage, ParseStats};

/// Env override for the transcripts root (dev/testing); production resolves
/// `~/.claude/projects` via [`projects_root`].
pub const PROJECTS_DIR_ENV: &str = "CLAUDE_USAGE_TRACKER_PROJECTS_DIR";

/// Outcome of one backfill pass, surfaced by `backfill_status` and the 3.5
/// diff report.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct BackfillSummary {
    /// `.jsonl` files discovered under the projects root.
    pub files_discovered: u64,
    /// Files that had bytes past their stored offset this pass.
    pub files_read: u64,
    /// Files whose stored offset exceeded the current file size
    /// (truncated/replaced) and were re-read from 0.
    pub files_reset: u64,
    /// Collapsed transcript requests examined this pass.
    pub requests_seen: u64,
    /// New `requests` rows inserted (`source='backfill'`).
    pub requests_inserted: u64,
    /// Requests already stored (either source); skipped per otel-wins.
    pub requests_deduped: u64,
    /// Existing rows whose missing 5m/1h cache split was filled in.
    pub splits_filled: u64,
    /// Inserted rows with `cost_usd = NULL` (model not in pricing table).
    pub unknown_model_rows: u64,
    /// Sessions created from transcript data (`source='backfill'`).
    pub sessions_created: u64,
    /// Existing sessions whose missing `cwd` was healed.
    pub sessions_healed: u64,
    /// Files/directories that could not be read or stored; logged and
    /// skipped, never fatal.
    pub io_errors: u64,
    /// Aggregated line accounting across every file read.
    pub parse: ParseStats,
    pub started_ms: i64,
    pub finished_ms: i64,
}

/// Point-in-time backfill state for the frontend (health view / 3.5).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct BackfillInfo {
    /// `true` while a pass is executing.
    pub running: bool,
    /// The most recent completed pass, if any.
    pub last: Option<BackfillSummary>,
}

/// Tauri-managed backfill state, shared between the startup pass, the
/// `backfill_status` command, and 3.5's manual trigger.
#[derive(Clone, Default)]
pub struct BackfillState(pub Arc<Mutex<BackfillInfo>>);

/// Query the backfill state from the frontend.
#[tauri::command]
pub fn backfill_status(state: tauri::State<'_, BackfillState>) -> BackfillInfo {
    state.0.lock().expect("backfill mutex poisoned").clone()
}

/// Resolve the transcripts root: the env override when set (dev/testing),
/// otherwise `~/.claude/projects`.
pub fn projects_root<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var(PROJECTS_DIR_ENV) {
        return Ok(PathBuf::from(path));
    }
    use tauri::Manager;
    app.path()
        .home_dir()
        .map(|home| home.join(".claude").join("projects"))
        .map_err(|err| format!("cannot resolve home directory: {err}"))
}

/// Run one full-or-incremental pass and record it in `state`. Blocking
/// (file I/O + DB writes); the startup caller runs it on a blocking thread.
pub fn run_pass(
    db: &Arc<Mutex<Db>>,
    pricing: &PricingState,
    state: &BackfillState,
    root: &Path,
) -> BackfillSummary {
    {
        let mut info = state.0.lock().expect("backfill mutex poisoned");
        info.running = true;
    }
    let summary = pass(db, pricing, root);
    let mut info = state.0.lock().expect("backfill mutex poisoned");
    info.running = false;
    info.last = Some(summary.clone());
    summary
}

/// The pass itself, without state bookkeeping. A missing root (fresh
/// machine, no Claude Code activity yet) is an empty pass, not an error.
fn pass(db: &Arc<Mutex<Db>>, pricing: &PricingState, root: &Path) -> BackfillSummary {
    let mut summary = BackfillSummary {
        started_ms: now_ms(),
        ..BackfillSummary::default()
    };

    let mut files = Vec::new();
    if root.is_dir() {
        discover_jsonl(root, &mut files, &mut summary.io_errors);
    }
    files.sort();
    summary.files_discovered = files.len() as u64;

    for path in &files {
        if let Err(err) = backfill_file(db, pricing, path, &mut summary) {
            summary.io_errors += 1;
            eprintln!("backfill: skipping {}: {err}", path.display());
        }
    }

    summary.finished_ms = now_ms();
    summary
}

/// Recursively collect `*.jsonl` files. Unreadable directories are counted
/// and skipped.
fn discover_jsonl(dir: &Path, files: &mut Vec<PathBuf>, io_errors: &mut u64) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            *io_errors += 1;
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            *io_errors += 1;
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            discover_jsonl(&path, files, io_errors);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
}

/// Errors that abort one file (never the pass).
#[derive(Debug)]
enum FileError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(err) => write!(f, "io error: {err}"),
            FileError::Sqlite(err) => write!(f, "sqlite error: {err}"),
        }
    }
}

impl From<std::io::Error> for FileError {
    fn from(err: std::io::Error) -> Self {
        FileError::Io(err)
    }
}

impl From<rusqlite::Error> for FileError {
    fn from(err: rusqlite::Error) -> Self {
        FileError::Sqlite(err)
    }
}

/// Process one transcript: parse from the stored offset, store requests and
/// session mappings, advance the offset. Rows and the offset commit in one
/// transaction so a crash mid-file re-reads (idempotently) rather than
/// losing data.
fn backfill_file(
    db: &Arc<Mutex<Db>>,
    pricing: &PricingState,
    path: &Path,
    summary: &mut BackfillSummary,
) -> Result<(), FileError> {
    let file_key = path.to_string_lossy().into_owned();
    let file_len = std::fs::metadata(path)?.len();

    let stored_offset = {
        let db = db.lock().expect("db mutex poisoned");
        stored_offset(db.conn(), &file_key)?
    };
    let offset = if file_len < stored_offset {
        summary.files_reset += 1;
        0
    } else {
        stored_offset
    };
    if file_len == offset {
        // Nothing new; don't even open the file.
        return Ok(());
    }

    // Parse outside the DB lock: this is the slow part of a full pass.
    let parse = transcript::parse_file_from(path, offset)?;
    let requests = transcript::collapse_requests(&parse.lines);
    summary.files_read += 1;
    summary.requests_seen += requests.len() as u64;
    merge_parse_stats(&mut summary.parse, &parse.stats);

    let db = db.lock().expect("db mutex poisoned");
    let tx = db.conn().unchecked_transaction()?;
    for request in &requests {
        store_request(&tx, pricing, request, summary)?;
    }
    store_sessions(&tx, &parse.lines, summary)?;
    tx.execute(
        "INSERT INTO ingest_state (file_path, byte_offset, updated_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (file_path) DO UPDATE SET
             byte_offset = excluded.byte_offset,
             updated_at_ms = excluded.updated_at_ms",
        rusqlite::params![file_key, (offset + parse.bytes_consumed) as i64, now_ms()],
    )?;
    tx.commit()?;
    Ok(())
}

fn stored_offset(conn: &Connection, file_key: &str) -> Result<u64, rusqlite::Error> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT byte_offset FROM ingest_state WHERE file_path = ?1",
            [file_key],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(offset.map(|o| o.max(0) as u64).unwrap_or(0))
}

/// Insert one collapsed transcript request, deferring to any existing row
/// per the otel-wins rule (which also makes re-reads after an offset reset
/// idempotent). The 5m/1h split is the only thing backfill may add to an
/// existing row — and only where it is NULL.
fn store_request(
    conn: &Connection,
    pricing: &PricingState,
    request: &AssistantUsage,
    summary: &mut BackfillSummary,
) -> Result<(), rusqlite::Error> {
    // collapse_requests drops id-less lines, but stay defensive.
    let Some(request_id) = request.request_id.as_deref() else {
        return Ok(());
    };

    let existing: Option<(Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT cache_creation_5m_tokens, cache_creation_1h_tokens
             FROM requests WHERE request_id = ?1",
            [request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    if let Some((existing_5m, existing_1h)) = existing {
        summary.requests_deduped += 1;
        let fills_5m = existing_5m.is_none() && request.cache_creation_5m_tokens.is_some();
        let fills_1h = existing_1h.is_none() && request.cache_creation_1h_tokens.is_some();
        if fills_5m || fills_1h {
            conn.execute(
                "UPDATE requests SET
                     cache_creation_5m_tokens =
                         COALESCE(cache_creation_5m_tokens, ?2),
                     cache_creation_1h_tokens =
                         COALESCE(cache_creation_1h_tokens, ?3)
                 WHERE request_id = ?1",
                rusqlite::params![
                    request_id,
                    request.cache_creation_5m_tokens,
                    request.cache_creation_1h_tokens,
                ],
            )?;
            summary.splits_filled += 1;
        }
        return Ok(());
    }

    let cost = pricing
        .cost_for(request.model.as_deref(), &UsageTokens::from(request))
        .usd();
    if cost.is_none() {
        summary.unknown_model_rows += 1;
    }
    conn.execute(
        "INSERT INTO requests (
            request_id, session_id, timestamp_ms, model, cost_usd,
            input_tokens, output_tokens, cache_read_tokens,
            cache_creation_tokens, cache_creation_5m_tokens,
            cache_creation_1h_tokens, event_type, source
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                   'api_request', 'backfill')",
        rusqlite::params![
            request_id,
            request.session_id,
            request.timestamp_ms,
            request.model,
            cost,
            request.input_tokens,
            request.output_tokens,
            request.cache_read_tokens,
            request.cache_creation_tokens,
            request.cache_creation_5m_tokens,
            request.cache_creation_1h_tokens,
        ],
    )?;
    summary.requests_inserted += 1;
    Ok(())
}

/// Upsert the session mappings observed in this parse: create sessions the
/// hook never reported (`source='backfill'`), fill a missing `cwd`, and
/// widen first/last-seen. Hook data is never overwritten and `source`
/// never downgrades from `'hook'`. Uses all assistant lines (synthetic ones
/// included): they all carry `sessionId`/`cwd`/`timestamp`.
fn store_sessions(
    conn: &Connection,
    lines: &[AssistantUsage],
    summary: &mut BackfillSummary,
) -> Result<(), rusqlite::Error> {
    struct SessionAgg<'a> {
        session_id: &'a str,
        cwd: Option<&'a str>,
        first_ms: i64,
        last_ms: i64,
    }

    let mut aggregates: Vec<SessionAgg> = Vec::new();
    for line in lines {
        match aggregates
            .iter_mut()
            .find(|agg| agg.session_id == line.session_id)
        {
            None => aggregates.push(SessionAgg {
                session_id: &line.session_id,
                cwd: line.cwd.as_deref(),
                first_ms: line.timestamp_ms,
                last_ms: line.timestamp_ms,
            }),
            Some(agg) => {
                agg.cwd = agg.cwd.or(line.cwd.as_deref());
                agg.first_ms = agg.first_ms.min(line.timestamp_ms);
                agg.last_ms = agg.last_ms.max(line.timestamp_ms);
            }
        }
    }

    for agg in &aggregates {
        let existing_cwd: Option<Option<String>> = conn
            .query_row(
                "SELECT cwd FROM sessions WHERE session_id = ?1",
                [agg.session_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        conn.execute(
            "INSERT INTO sessions (session_id, cwd, first_seen_ms, last_seen_ms, source)
             VALUES (?1, ?2, ?3, ?4, 'backfill')
             ON CONFLICT (session_id) DO UPDATE SET
                 cwd = COALESCE(sessions.cwd, excluded.cwd),
                 first_seen_ms = MIN(sessions.first_seen_ms, excluded.first_seen_ms),
                 last_seen_ms = MAX(COALESCE(sessions.last_seen_ms, excluded.last_seen_ms),
                                    excluded.last_seen_ms)",
            rusqlite::params![agg.session_id, agg.cwd, agg.first_ms, agg.last_ms],
        )?;
        match existing_cwd {
            None => summary.sessions_created += 1,
            Some(None) if agg.cwd.is_some() => summary.sessions_healed += 1,
            Some(_) => {}
        }
    }
    Ok(())
}

fn merge_parse_stats(total: &mut ParseStats, stats: &ParseStats) {
    total.lines_read += stats.lines_read;
    total.assistant_lines += stats.assistant_lines;
    total.skipped_lines += stats.skipped_lines;
    total.malformed_lines += stats.malformed_lines;
    total.invalid_assistant_lines += stats.invalid_assistant_lines;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::PricingTable;

    const MAIN_SESSION: &str = include_str!("../tests/fixtures/transcripts/main-session.jsonl");
    const SIDECHAIN: &str = include_str!("../tests/fixtures/transcripts/sidechain.jsonl");
    const EDGE_CASES: &str = include_str!("../tests/fixtures/transcripts/edge-cases.jsonl");

    /// Fixture layout mirrors the real tree: per-project dirs with session
    /// files plus a `subagents` subdir for the sidechain transcript.
    fn fixture_root(dir: &Path) -> PathBuf {
        let root = dir.join("projects");
        let project = root.join("-Users-peason-Projects-luxurypresence-luxp");
        let subagents = root
            .join("-Users-peason-Projects-ensemble")
            .join("56594d25-94a4-4449-9a7a-a3c654b5c4a3")
            .join("subagents");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            project.join("5e6aa3df-f340-46ad-8c40-d613f7073b97.jsonl"),
            MAIN_SESSION,
        )
        .unwrap();
        std::fs::write(subagents.join("agent-acb24f2158e2fb8a9.jsonl"), SIDECHAIN).unwrap();
        // A stray non-transcript file must be ignored.
        std::fs::write(root.join("README.md"), "not a transcript").unwrap();
        root
    }

    fn test_env() -> (
        Arc<Mutex<Db>>,
        PricingState,
        BackfillState,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()));
        let pricing = PricingState::new(PricingTable::bundled());
        (db, pricing, BackfillState::default(), dir)
    }

    fn count(db: &Arc<Mutex<Db>>, sql: &str) -> i64 {
        let db = db.lock().unwrap();
        db.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn fresh_pass_ingests_all_transcripts_and_rerun_is_idempotent() {
        let (db, pricing, state, dir) = test_env();
        let root = fixture_root(dir.path());

        // First pass: 2 main-session + 2 sidechain requests.
        let first = run_pass(&db, &pricing, &state, &root);
        assert_eq!(first.files_discovered, 2);
        assert_eq!(first.files_read, 2);
        assert_eq!(first.requests_seen, 4);
        assert_eq!(first.requests_inserted, 4);
        assert_eq!(first.requests_deduped, 0);
        assert_eq!(first.unknown_model_rows, 0);
        assert_eq!(first.io_errors, 0);
        assert_eq!(first.parse.malformed_lines, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 4);
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM requests WHERE source = 'backfill'"
            ),
            4
        );
        // Costs computed for every known-model row.
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM requests WHERE cost_usd IS NOT NULL"
            ),
            4
        );
        // Both sessions created from transcript data.
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM sessions WHERE source = 'backfill'"
            ),
            2
        );

        // Second pass: offsets make it a no-op (files not even read).
        let second = run_pass(&db, &pricing, &state, &root);
        assert_eq!(second.files_discovered, 2);
        assert_eq!(second.files_read, 0);
        assert_eq!(second.requests_inserted, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 4);

        // State carries the last summary.
        let info = state.0.lock().unwrap().clone();
        assert!(!info.running);
        assert_eq!(info.last, Some(second));
    }

    #[test]
    fn fixture_costs_match_the_pricing_table() {
        let (db, pricing, state, dir) = test_env();
        let root = fixture_root(dir.path());
        run_pass(&db, &pricing, &state, &root);

        // Hand-verified in task 3.3 for the main session's two requests.
        let db = db.lock().unwrap();
        let cost: f64 = db
            .conn()
            .query_row(
                "SELECT cost_usd FROM requests
                 WHERE request_id = 'req_011Cbwf9sGnBjoiZz25k4EK8'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((cost - 0.825_931).abs() < 1e-9, "got {cost}");
        let cost: f64 = db
            .conn()
            .query_row(
                "SELECT cost_usd FROM requests
                 WHERE request_id = 'req_011CbwfAFuopq3NdmbdDHmd2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((cost - 0.567_567).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn simulated_gap_recovers_missed_requests_without_double_counting() {
        let (db, pricing, state, dir) = test_env();
        let root = fixture_root(dir.path());

        // While the app was up, the live pipeline stored request 1 of the
        // main session (otel cost is authoritative). Request 2 happened
        // while the app was down.
        {
            let db = db.lock().unwrap();
            db.conn()
                .execute(
                    "INSERT INTO requests (
                        request_id, session_id, timestamp_ms, model, cost_usd,
                        input_tokens, output_tokens, cache_read_tokens,
                        cache_creation_tokens, source
                     ) VALUES ('req_011Cbwf9sGnBjoiZz25k4EK8',
                               '5e6aa3df-f340-46ad-8c40-d613f7073b97',
                               1781189404478, 'claude-fable-5', 0.99,
                               17045, 94, 23661, 31356, 'otel')",
                    [],
                )
                .unwrap();
        }

        let summary = run_pass(&db, &pricing, &state, &root);
        assert_eq!(summary.requests_inserted, 3); // main #2 + 2 sidechain
        assert_eq!(summary.requests_deduped, 1);
        // The otel row gained the transcript-exclusive split…
        assert_eq!(summary.splits_filled, 1);

        let (source, cost, split_1h): (String, f64, i64) = {
            let db = db.lock().unwrap();
            db.conn()
                .query_row(
                    "SELECT source, cost_usd, cache_creation_1h_tokens FROM requests
                     WHERE request_id = 'req_011Cbwf9sGnBjoiZz25k4EK8'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        // …but otherwise stayed otel-authoritative (cost untouched).
        assert_eq!(source, "otel");
        assert!((cost - 0.99).abs() < 1e-12);
        assert_eq!(split_1h, 31_356);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 4);
    }

    #[test]
    fn incremental_pass_picks_up_lines_appended_after_the_offset() {
        let (db, pricing, state, dir) = test_env();
        let root = dir.path().join("projects");
        let project = root.join("-p");
        std::fs::create_dir_all(&project).unwrap();
        let file = project.join("5e6aa3df-f340-46ad-8c40-d613f7073b97.jsonl");

        // First pass sees only the first streaming group (request 1's two
        // lines are 14-15; split at a line boundary right after them).
        let split_at = MAIN_SESSION
            .lines()
            .take(15)
            .map(|l| l.len() + 1)
            .sum::<usize>();
        std::fs::write(&file, &MAIN_SESSION[..split_at]).unwrap();
        let first = run_pass(&db, &pricing, &state, &root);
        assert_eq!(first.requests_inserted, 1);

        // Claude Code appends the rest of the session while the app is off.
        std::fs::write(&file, MAIN_SESSION).unwrap();
        let second = run_pass(&db, &pricing, &state, &root);
        assert_eq!(second.files_read, 1);
        assert_eq!(second.requests_inserted, 1); // only request 2 is new
        assert_eq!(second.requests_deduped, 0); // request 1's lines not re-read
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 2);
    }

    #[test]
    fn truncated_file_resets_offset_and_re_reads_idempotently() {
        let (db, pricing, state, dir) = test_env();
        let root = dir.path().join("projects");
        let project = root.join("-p");
        std::fs::create_dir_all(&project).unwrap();
        let file = project.join("s.jsonl");

        std::fs::write(&file, MAIN_SESSION).unwrap();
        run_pass(&db, &pricing, &state, &root);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 2);

        // The file is replaced by a shorter one (rotation/cleanup): offset
        // resets and the overlap (request 1, lines 14-15) dedupes instead
        // of double-counting.
        let shorter: String = MAIN_SESSION
            .lines()
            .take(15)
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
        std::fs::write(&file, &shorter).unwrap();
        let pass = run_pass(&db, &pricing, &state, &root);
        assert_eq!(pass.files_reset, 1);
        assert_eq!(pass.requests_inserted, 0);
        assert_eq!(pass.requests_deduped, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 2);
    }

    #[test]
    fn sessions_missing_cwd_are_healed_and_hook_data_is_preserved() {
        let (db, pricing, state, dir) = test_env();
        let root = fixture_root(dir.path());

        // The hook POST for the main session arrived without cwd (or was
        // partially recorded); the ensemble session has a hook row whose
        // cwd must NOT be overwritten.
        {
            let db = db.lock().unwrap();
            db.conn()
                .execute(
                    "INSERT INTO sessions (session_id, cwd, first_seen_ms, source)
                     VALUES ('5e6aa3df-f340-46ad-8c40-d613f7073b97', NULL, 1781189400000, 'hook'),
                            ('56594d25-94a4-4449-9a7a-a3c654b5c4a3', '/hook/cwd', 1779989700000, 'hook')",
                    [],
                )
                .unwrap();
        }

        let summary = run_pass(&db, &pricing, &state, &root);
        assert_eq!(summary.sessions_created, 0);
        assert_eq!(summary.sessions_healed, 1);

        let db = db.lock().unwrap();
        let (cwd, source): (String, String) = db
            .conn()
            .query_row(
                "SELECT cwd, source FROM sessions
                 WHERE session_id = '5e6aa3df-f340-46ad-8c40-d613f7073b97'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cwd, "/Users/peason/Projects/luxurypresence/luxp");
        assert_eq!(source, "hook"); // never downgraded to backfill
        let cwd: String = db
            .conn()
            .query_row(
                "SELECT cwd FROM sessions
                 WHERE session_id = '56594d25-94a4-4449-9a7a-a3c654b5c4a3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cwd, "/hook/cwd"); // hook cwd wins over transcript
    }

    #[test]
    fn edge_case_lines_unknown_model_rows_store_null_cost() {
        let (db, pricing, state, dir) = test_env();
        let root = dir.path().join("projects");
        let project = root.join("-p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("s.jsonl"), EDGE_CASES).unwrap();
        // One extra line with a model the pricing table cannot know.
        let unknown = concat!(
            r#"{"type":"assistant","sessionId":"sess-u","timestamp":"2026-06-11T10:00:00Z","#,
            r#""requestId":"req_unknown_model","#,
            r#""message":{"model":"claude-12-quantum","usage":{"input_tokens":5,"output_tokens":9}}}"#,
            "\n",
        );
        std::fs::write(project.join("u.jsonl"), unknown).unwrap();

        let summary = run_pass(&db, &pricing, &state, &root);
        // Edge-cases file: 2 collapsed requests (synthetic dropped) + 1 here.
        assert_eq!(summary.requests_inserted, 3);
        assert_eq!(summary.unknown_model_rows, 1);
        assert_eq!(summary.parse.malformed_lines, 1); // tolerated, not fatal

        let db = db.lock().unwrap();
        let cost: Option<f64> = db
            .conn()
            .query_row(
                "SELECT cost_usd FROM requests WHERE request_id = 'req_unknown_model'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cost, None); // tokens-only flag per 3.3 contract
    }

    #[test]
    fn missing_root_is_an_empty_pass_not_an_error() {
        let (db, pricing, state, dir) = test_env();
        let summary = run_pass(&db, &pricing, &state, &dir.path().join("never-created"));
        assert_eq!(summary.files_discovered, 0);
        assert_eq!(summary.io_errors, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM requests"), 0);
        assert!(state.0.lock().unwrap().last.is_some());
    }

    #[test]
    fn backfill_info_serializes_for_frontend() {
        let info = BackfillInfo {
            running: false,
            last: Some(BackfillSummary::default()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["running"], false);
        assert_eq!(json["last"]["files_discovered"], 0);
        assert_eq!(json["last"]["parse"]["lines_read"], 0);
    }
}
