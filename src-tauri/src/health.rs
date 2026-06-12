//! Health & diagnostics backend (task 2.5).
//!
//! One read-only command ([`health_status`]) that aggregates everything the
//! health view renders:
//!
//! - receiver lifecycle state ([`crate::receiver::ReceiverStatus`]),
//!   including the port-conflict case that is never auto-rebound
//! - settings.json config state: installed / missing / conflicting / error
//!   (derived live from the file via [`crate::settings_merge`])
//! - last event received: the freshest of the in-memory ingest wall clock
//!   (this launch) and the newest stored `requests` row (survives restarts)
//! - the ingest counters from task 1.4 (`events_ingested`,
//!   `ingest_failures`, `events_skipped`)
//! - backfill progress: the live [`BackfillInfo`] (running flag + last pass
//!   summary) from the Epic 3 engine
//!
//! It also runs the "configured but no events" detector: when the config is
//! installed but nothing has arrived in [`NO_EVENTS_THRESHOLD_MINUTES`], the
//! status carries a [`NoEventsDiagnosis`] listing the likely causes (port
//! conflict, receiver failure, sessions predating the config, or simply no
//! Claude Code activity).

use std::path::Path;

use serde::Serialize;
use tauri::{Manager, Runtime};

use crate::backfill::{BackfillInfo, BackfillState};
use crate::db::Db;
use crate::ingest::{IngestState, IngestStatsSnapshot};
use crate::receiver::{ReceiverState, ReceiverStatus};
use crate::settings_merge::{
    describe_settings_error, detect_conflicts, is_installed, read_settings, Conflict,
};

/// How long the app waits, with config installed, before flagging the
/// "configured but no events" state. Long enough that one slow human turn
/// in an active session doesn't flap the warning; short enough to be useful
/// while debugging a fresh install.
pub const NO_EVENTS_THRESHOLD_MINUTES: i64 = 10;

/// settings.json config state as the health view reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConfigState {
    /// All app env keys + the SessionStart hook are present, no conflicts.
    Installed,
    /// App config absent or partial (covers a missing file too). Telemetry
    /// is not flowing; the fix is re-running onboarding.
    Missing,
    /// Pre-existing telemetry config detected. `installed` distinguishes
    /// "ours is in place but foreign OTel keys coexist" from "not set up
    /// and conflicting".
    Conflicting {
        installed: bool,
        conflicts: Vec<Conflict>,
    },
    /// settings.json could not be read/parsed (malformed JSON, IO error).
    Error { message: String },
}

impl ConfigState {
    /// Whether the app's export config is in place (events should flow).
    fn is_configured(&self) -> bool {
        matches!(
            self,
            ConfigState::Installed
                | ConfigState::Conflicting {
                    installed: true,
                    ..
                }
        )
    }
}

/// Compute the config state from the settings file. Read-only.
pub fn config_state(settings_path: &Path) -> ConfigState {
    let current = match read_settings(settings_path) {
        Ok(map) => map,
        Err(err) => {
            return ConfigState::Error {
                message: describe_settings_error(&err, settings_path),
            }
        }
    };
    let installed = is_installed(&current);
    let conflicts = detect_conflicts(&current);
    if !conflicts.is_empty() {
        ConfigState::Conflicting {
            installed,
            conflicts,
        }
    } else if installed {
        ConfigState::Installed
    } else {
        ConfigState::Missing
    }
}

/// One likely cause in a [`NoEventsDiagnosis`], with a stable machine kind
/// and a human remediation string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cause {
    /// `"capture_paused"`, `"port_conflict"`, `"receiver_failed"`,
    /// `"receiver_starting"`, `"sessions_predate_config"`, or `"idle"`.
    pub kind: &'static str,
    pub detail: String,
}

/// The "configured but no events in N minutes" state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoEventsDiagnosis {
    pub threshold_minutes: i64,
    /// Minutes since the last event; `None` when none was ever received.
    pub minutes_since_last: Option<i64>,
    /// Likely causes, most definitive first.
    pub causes: Vec<Cause>,
}

/// Run the no-events detector. Fires only when the config is installed
/// (events *should* flow) and nothing has arrived within the threshold.
/// Paused capture or a broken receiver is the definitive cause when
/// present; with a healthy receiver the causes are the ambiguous pair the
/// user must check (pre-config sessions still running, or Claude Code
/// simply not in use).
pub fn diagnose_no_events(
    config: &ConfigState,
    receiver: &ReceiverStatus,
    capture_paused: bool,
    last_event_ms: Option<i64>,
    now_ms: i64,
) -> Option<NoEventsDiagnosis> {
    if !config.is_configured() {
        return None;
    }
    let minutes_since_last = last_event_ms.map(|t| (now_ms - t) / 60_000);
    if minutes_since_last.is_some_and(|m| m < NO_EVENTS_THRESHOLD_MINUTES) {
        return None;
    }
    if capture_paused {
        return Some(NoEventsDiagnosis {
            threshold_minutes: NO_EVENTS_THRESHOLD_MINUTES,
            minutes_since_last,
            causes: vec![Cause {
                kind: "capture_paused",
                detail: "Capture is paused, so incoming events are acknowledged but \
                         discarded. Resume capture to store events again; the paused \
                         window can be recovered later with a backfill pass."
                    .to_string(),
            }],
        });
    }
    let causes = match receiver {
        ReceiverStatus::PortInUse { port } => vec![Cause {
            kind: "port_conflict",
            detail: format!(
                "Another process is holding port {port}, so Claude Code cannot deliver events. \
                 Quit whatever is using the port and relaunch this app."
            ),
        }],
        ReceiverStatus::Failed { message } => vec![Cause {
            kind: "receiver_failed",
            detail: format!(
                "The receiver stopped and is not accepting events: {message}. Relaunch this app."
            ),
        }],
        ReceiverStatus::Starting => vec![Cause {
            kind: "receiver_starting",
            detail: "The receiver is still starting up; check again in a moment.".to_string(),
        }],
        ReceiverStatus::Listening { .. } => vec![
            Cause {
                kind: "sessions_predate_config",
                detail: "Claude Code sessions started before setup never export telemetry. \
                         Restart any sessions that are still running."
                    .to_string(),
            },
            Cause {
                kind: "idle",
                detail: "Claude Code may simply not be in use right now. No usage means no \
                         events; this is normal."
                    .to_string(),
            },
        ],
    };
    Some(NoEventsDiagnosis {
        threshold_minutes: NO_EVENTS_THRESHOLD_MINUTES,
        minutes_since_last,
        causes,
    })
}

/// The transcripts root the backfill engine reads, and whether it exists
/// (a fresh machine has none until the first Claude Code session; the
/// health view explains that instead of showing a bare "0 files").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptsInfo {
    /// Display path of the transcripts root (`~/.claude/projects`).
    pub path: String,
    pub exists: bool,
}

/// Everything the health view renders, in one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthStatus {
    pub receiver: ReceiverStatus,
    pub config: ConfigState,
    /// Display path of the settings file the config state was read from.
    pub settings_path: String,
    /// Capture pause state (task 4.4): while `true`, arriving events are
    /// acknowledged and discarded, which the health view must say out loud.
    pub capture_paused: bool,
    /// Since-launch ingest counters (task 1.4), including `ingest_failures`
    /// and the most recent failure detail.
    pub ingest: IngestStatsSnapshot,
    /// Unix ms of the most recent event received; `None` when none ever.
    /// Freshest of the in-memory ingest clock and the stored rows, so it
    /// survives app restarts.
    pub last_event_ms: Option<i64>,
    /// All-time `requests` rows received live (`source = 'otel'`).
    pub events_stored: u64,
    /// Set when the stored-event totals could not be read from the
    /// database (locked by another process, disk trouble). The rest of the
    /// snapshot stays usable; `events_stored`/`last_event_ms` fall back to
    /// the in-memory counters only.
    pub db_error: Option<String>,
    /// Transcripts root used by backfill, and whether it exists.
    pub transcripts: TranscriptsInfo,
    /// Transcript backfill: running flag + the last completed pass.
    pub backfill: BackfillInfo,
    /// Present when the "configured but no events" detector fired.
    pub no_events: Option<NoEventsDiagnosis>,
}

/// Count and newest event time of live-received (`source='otel'`) rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoredEvents {
    pub count: u64,
    /// Event time of the newest stored row; `None` when no rows exist.
    pub last_event_ms: Option<i64>,
}

/// Pure assembly of [`HealthStatus`] from its inputs (testable without an
/// app handle). `stored.last_event_ms` is event time from stored rows;
/// `ingest.last_event_ms` is the wall clock of the last live ingest (0 =
/// never this launch). The freshest of the two is "last event received".
#[allow(clippy::too_many_arguments)]
pub fn compute_health(
    receiver: ReceiverStatus,
    config: ConfigState,
    settings_path: String,
    capture_paused: bool,
    ingest: IngestStatsSnapshot,
    stored: Result<StoredEvents, String>,
    transcripts: TranscriptsInfo,
    backfill: BackfillInfo,
    now_ms: i64,
) -> HealthStatus {
    // A failed stored-events read (locked database, disk trouble) degrades
    // to the in-memory since-launch counters instead of taking the whole
    // health view down with it (task 6.4).
    let (stored, db_error) = match stored {
        Ok(stored) => (stored, None),
        Err(message) => (
            StoredEvents {
                count: ingest.events_ingested,
                last_event_ms: None,
            },
            Some(message),
        ),
    };
    let last_event_ms = [
        (ingest.last_event_ms > 0).then_some(ingest.last_event_ms),
        stored.last_event_ms,
    ]
    .into_iter()
    .flatten()
    .max();
    let no_events = diagnose_no_events(&config, &receiver, capture_paused, last_event_ms, now_ms);
    HealthStatus {
        receiver,
        config,
        settings_path,
        capture_paused,
        ingest,
        last_event_ms,
        events_stored: stored.count,
        db_error,
        transcripts,
        backfill,
        no_events,
    }
}

/// Query the [`StoredEvents`] aggregate.
fn db_event_stats(db: &Db) -> Result<StoredEvents, rusqlite::Error> {
    db.conn().query_row(
        "SELECT COUNT(*), MAX(timestamp_ms) FROM requests WHERE source = 'otel'",
        [],
        |row| {
            Ok(StoredEvents {
                count: row.get::<_, i64>(0)? as u64,
                last_event_ms: row.get(1)?,
            })
        },
    )
}

/// Gather live state from the managed receiver/ingest state and the
/// settings file, then assemble the status. Read-only everywhere.
pub fn current_health<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<HealthStatus, String> {
    let receiver = app
        .state::<ReceiverState>()
        .0
        .lock()
        .expect("receiver status mutex poisoned")
        .clone();
    let ingest_state = app.state::<IngestState>();
    let ingest = ingest_state.stats.snapshot();
    let stored = {
        let db = ingest_state.db.lock().expect("db mutex poisoned");
        db_event_stats(&db).map_err(|err| {
            format!(
                "The usage database could not be read ({err}). Totals shown are \
                 since-launch only. If another copy of this app is running, quit it; \
                 otherwise check free disk space and relaunch."
            )
        })
    };
    let capture_paused = app
        .try_state::<crate::capture::CaptureState>()
        .map(|state| state.paused())
        .unwrap_or(false);
    let transcripts = {
        let root = crate::backfill::projects_root(app)?;
        TranscriptsInfo {
            exists: root.is_dir(),
            path: root.display().to_string(),
        }
    };
    let backfill = app
        .state::<BackfillState>()
        .0
        .lock()
        .expect("backfill mutex poisoned")
        .clone();
    let settings_path = crate::onboarding::settings_path(app)?;
    let config = config_state(&settings_path);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(compute_health(
        receiver,
        config,
        settings_path.display().to_string(),
        capture_paused,
        ingest,
        stored,
        transcripts,
        backfill,
        now_ms,
    ))
}

/// Frontend query: the full diagnostics snapshot for the health view.
#[tauri::command]
pub fn health_status<R: Runtime>(app: tauri::AppHandle<R>) -> Result<HealthStatus, String> {
    current_health(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use crate::settings_merge::{apply_merge, merge_file};
    use serde_json::{json, Map, Value};
    use tempfile::TempDir;

    const PREEXISTING_ENV: &str = include_str!("../tests/fixtures/settings/preexisting_env.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/settings/malformed.json");

    const MINUTE_MS: i64 = 60_000;
    const NOW_MS: i64 = 1_781_200_000_000;

    fn listening() -> ReceiverStatus {
        ReceiverStatus::Listening { port: 43177 }
    }

    fn snapshot(last_event_ms: i64) -> IngestStatsSnapshot {
        IngestStatsSnapshot {
            events_ingested: 0,
            ingest_failures: 0,
            events_skipped: 0,
            last_event_ms,
            last_failure: None,
        }
    }

    fn transcripts() -> TranscriptsInfo {
        TranscriptsInfo {
            path: "/tmp/projects".into(),
            exists: true,
        }
    }

    fn write_settings(dir: &TempDir, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join("settings.json");
        std::fs::write(&path, contents).expect("write settings");
        path
    }

    fn installed_settings(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("settings.json");
        merge_file(&path, &dir.path().join("backups")).expect("merge");
        path
    }

    // ---- config_state ----

    #[test]
    fn missing_file_and_unconfigured_file_are_missing() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            config_state(&dir.path().join("settings.json")),
            ConfigState::Missing
        );
        let path = write_settings(&dir, r#"{"model": "sonnet"}"#);
        assert_eq!(config_state(&path), ConfigState::Missing);
    }

    #[test]
    fn merged_file_is_installed() {
        let dir = TempDir::new().unwrap();
        let path = installed_settings(&dir);
        assert_eq!(config_state(&path), ConfigState::Installed);
    }

    #[test]
    fn preexisting_telemetry_is_conflicting_not_installed() {
        let dir = TempDir::new().unwrap();
        let path = write_settings(&dir, PREEXISTING_ENV);
        match config_state(&path) {
            ConfigState::Conflicting {
                installed,
                conflicts,
            } => {
                assert!(!installed);
                assert_eq!(conflicts.len(), 3);
            }
            other => panic!("expected Conflicting, got {other:?}"),
        }
    }

    #[test]
    fn installed_with_foreign_otel_key_is_conflicting_installed() {
        // App config fully in place, but the user also exports elsewhere.
        let mut merged = apply_merge(&Map::new()).unwrap();
        merged["env"]["OTEL_METRICS_EXPORTER"] = Value::String("otlp".into());
        let dir = TempDir::new().unwrap();
        let path = write_settings(
            &dir,
            &serde_json::to_string_pretty(&Value::Object(merged)).unwrap(),
        );
        match config_state(&path) {
            ConfigState::Conflicting {
                installed,
                conflicts,
            } => {
                assert!(installed);
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].key, "OTEL_METRICS_EXPORTER");
            }
            other => panic!("expected Conflicting, got {other:?}"),
        }
    }

    #[test]
    fn malformed_file_is_error() {
        let dir = TempDir::new().unwrap();
        let path = write_settings(&dir, MALFORMED);
        match config_state(&path) {
            ConfigState::Error { message } => {
                assert!(message.contains("not valid JSON"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ---- diagnose_no_events ----

    #[test]
    fn detector_silent_when_not_configured() {
        for config in [
            ConfigState::Missing,
            ConfigState::Error {
                message: "nope".into(),
            },
            ConfigState::Conflicting {
                installed: false,
                conflicts: vec![],
            },
        ] {
            assert_eq!(
                diagnose_no_events(&config, &listening(), false, None, NOW_MS),
                None,
                "must not fire for {config:?}"
            );
        }
    }

    #[test]
    fn detector_silent_with_recent_event() {
        let recent = NOW_MS - (NO_EVENTS_THRESHOLD_MINUTES - 1) * MINUTE_MS;
        assert_eq!(
            diagnose_no_events(
                &ConfigState::Installed,
                &listening(),
                false,
                Some(recent),
                NOW_MS
            ),
            None
        );
        // A recent event silences the detector even while paused.
        assert_eq!(
            diagnose_no_events(
                &ConfigState::Installed,
                &listening(),
                true,
                Some(recent),
                NOW_MS
            ),
            None
        );
    }

    #[test]
    fn detector_fires_with_stale_event_and_healthy_receiver() {
        let stale = NOW_MS - 45 * MINUTE_MS;
        let diagnosis = diagnose_no_events(
            &ConfigState::Installed,
            &listening(),
            false,
            Some(stale),
            NOW_MS,
        )
        .expect("must fire");
        assert_eq!(diagnosis.minutes_since_last, Some(45));
        assert_eq!(diagnosis.threshold_minutes, NO_EVENTS_THRESHOLD_MINUTES);
        let kinds: Vec<&str> = diagnosis.causes.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, ["sessions_predate_config", "idle"]);
    }

    /// Paused capture is the definitive cause: it trumps everything,
    /// including a broken receiver, because resuming is the fix either way.
    #[test]
    fn paused_capture_is_the_definitive_cause() {
        for receiver in [listening(), ReceiverStatus::PortInUse { port: 43177 }] {
            let diagnosis =
                diagnose_no_events(&ConfigState::Installed, &receiver, true, None, NOW_MS)
                    .expect("must fire");
            assert_eq!(diagnosis.causes.len(), 1);
            assert_eq!(diagnosis.causes[0].kind, "capture_paused");
            assert!(
                diagnosis.causes[0].detail.contains("Resume"),
                "remediation must say how to fix it: {}",
                diagnosis.causes[0].detail
            );
        }
    }

    #[test]
    fn detector_fires_when_no_event_ever() {
        let diagnosis =
            diagnose_no_events(&ConfigState::Installed, &listening(), false, None, NOW_MS)
                .expect("must fire");
        assert_eq!(diagnosis.minutes_since_last, None);
        assert!(!diagnosis.causes.is_empty());
    }

    #[test]
    fn detector_fires_for_installed_config_with_foreign_conflicts() {
        let config = ConfigState::Conflicting {
            installed: true,
            conflicts: vec![],
        };
        assert!(diagnose_no_events(&config, &listening(), false, None, NOW_MS).is_some());
    }

    #[test]
    fn port_conflict_is_the_definitive_cause() {
        let receiver = ReceiverStatus::PortInUse { port: 43177 };
        let diagnosis = diagnose_no_events(&ConfigState::Installed, &receiver, false, None, NOW_MS)
            .expect("must fire");
        assert_eq!(diagnosis.causes.len(), 1);
        assert_eq!(diagnosis.causes[0].kind, "port_conflict");
        assert!(diagnosis.causes[0].detail.contains("43177"));
    }

    #[test]
    fn failed_receiver_is_the_definitive_cause() {
        let receiver = ReceiverStatus::Failed {
            message: "boom".into(),
        };
        let diagnosis = diagnose_no_events(&ConfigState::Installed, &receiver, false, None, NOW_MS)
            .expect("must fire");
        assert_eq!(diagnosis.causes.len(), 1);
        assert_eq!(diagnosis.causes[0].kind, "receiver_failed");
        assert!(diagnosis.causes[0].detail.contains("boom"));
    }

    // ---- compute_health ----

    #[test]
    fn last_event_is_freshest_of_memory_and_db() {
        let fresher_memory = compute_health(
            listening(),
            ConfigState::Installed,
            "p".into(),
            false,
            snapshot(NOW_MS - MINUTE_MS),
            Ok(StoredEvents {
                count: 10,
                last_event_ms: Some(NOW_MS - 5 * MINUTE_MS),
            }),
            transcripts(),
            BackfillInfo::default(),
            NOW_MS,
        );
        assert_eq!(fresher_memory.last_event_ms, Some(NOW_MS - MINUTE_MS));
        assert_eq!(fresher_memory.no_events, None);
        assert_eq!(fresher_memory.db_error, None);

        // Restart case: nothing ingested this launch, rows in the DB.
        let db_only = compute_health(
            listening(),
            ConfigState::Installed,
            "p".into(),
            false,
            snapshot(0),
            Ok(StoredEvents {
                count: 10,
                last_event_ms: Some(NOW_MS - 5 * MINUTE_MS),
            }),
            transcripts(),
            BackfillInfo::default(),
            NOW_MS,
        );
        assert_eq!(db_only.last_event_ms, Some(NOW_MS - 5 * MINUTE_MS));

        let never = compute_health(
            listening(),
            ConfigState::Installed,
            "p".into(),
            false,
            snapshot(0),
            Ok(StoredEvents::default()),
            transcripts(),
            BackfillInfo::default(),
            NOW_MS,
        );
        assert_eq!(never.last_event_ms, None);
        assert!(never.no_events.is_some(), "configured + never = detector");
        assert_eq!(never.backfill, BackfillInfo::default());
    }

    /// A failed stored-events read (locked DB, disk trouble) degrades to
    /// the since-launch counters and carries the message, instead of
    /// erroring the whole health view (task 6.4).
    #[test]
    fn db_read_failure_degrades_with_message() {
        let mut ingest = snapshot(NOW_MS - MINUTE_MS);
        ingest.events_ingested = 7;
        let health = compute_health(
            listening(),
            ConfigState::Installed,
            "p".into(),
            false,
            ingest,
            Err("The usage database could not be read (locked)".into()),
            transcripts(),
            BackfillInfo::default(),
            NOW_MS,
        );
        assert_eq!(health.events_stored, 7, "falls back to in-memory counter");
        assert_eq!(health.last_event_ms, Some(NOW_MS - MINUTE_MS));
        let message = health.db_error.expect("db error surfaced");
        assert!(message.contains("could not be read"), "got: {message}");
    }

    // ---- command wiring over a real (mock-runtime) app ----

    #[test]
    fn current_health_reads_managed_state_and_settings_file() {
        let dir = TempDir::new().unwrap();
        let settings = installed_settings(&dir);

        // Real DB with one stored event row.
        let db = crate::db::Db::open_in_dir(dir.path()).unwrap();
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, source)
                 VALUES ('sess-1', 1781200718939, 'otel')",
                [],
            )
            .unwrap();
        let ingest = IngestState::new(Arc::new(Mutex::new(db)));

        let status_cell = crate::receiver::new_status();
        *status_cell.lock().unwrap() = ReceiverStatus::Listening { port: 43177 };

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(ReceiverState(status_cell));
        app.manage(ingest);
        let backfill_state = BackfillState::default();
        backfill_state.0.lock().unwrap().running = true;
        app.manage(backfill_state);

        // Point the settings resolution at the temp file (dev override).
        std::env::set_var(crate::onboarding::SETTINGS_PATH_ENV, &settings);
        let health = current_health(app.handle());
        std::env::remove_var(crate::onboarding::SETTINGS_PATH_ENV);

        let health = health.expect("health");
        assert_eq!(health.receiver, ReceiverStatus::Listening { port: 43177 });
        assert_eq!(health.config, ConfigState::Installed);
        assert_eq!(health.settings_path, settings.display().to_string());
        assert_eq!(health.events_stored, 1);
        assert_eq!(health.last_event_ms, Some(1_781_200_718_939));
        // Backfill state is read live from the managed BackfillState.
        assert!(health.backfill.running);
        assert_eq!(health.backfill.last, None);
        // That event is ancient relative to the real clock: detector fires.
        let diagnosis = health.no_events.expect("detector fired");
        assert!(diagnosis.minutes_since_last.unwrap() >= NO_EVENTS_THRESHOLD_MINUTES);
    }

    // ---- serialization contract for the frontend ----

    #[test]
    fn health_serializes_for_frontend() {
        let health = compute_health(
            ReceiverStatus::PortInUse { port: 43177 },
            ConfigState::Installed,
            "/tmp/settings.json".into(),
            false,
            IngestStatsSnapshot {
                events_ingested: 2,
                ingest_failures: 1,
                events_skipped: 3,
                last_event_ms: NOW_MS - 20 * MINUTE_MS,
                last_failure: Some("disk full".into()),
            },
            Ok(StoredEvents {
                count: 5,
                last_event_ms: None,
            }),
            TranscriptsInfo {
                path: "/tmp/projects".into(),
                exists: false,
            },
            BackfillInfo::default(),
            NOW_MS,
        );
        let value = serde_json::to_value(&health).unwrap();
        assert_eq!(
            value,
            json!({
                "receiver": {"state": "port_in_use", "port": 43177},
                "config": {"state": "installed"},
                "settings_path": "/tmp/settings.json",
                "capture_paused": false,
                "ingest": {
                    "events_ingested": 2,
                    "ingest_failures": 1,
                    "events_skipped": 3,
                    "last_event_ms": NOW_MS - 20 * MINUTE_MS,
                    "last_failure": "disk full",
                },
                "last_event_ms": NOW_MS - 20 * MINUTE_MS,
                "events_stored": 5,
                "db_error": null,
                "transcripts": {"path": "/tmp/projects", "exists": false},
                "backfill": {"running": false, "last": null},
                "no_events": {
                    "threshold_minutes": NO_EVENTS_THRESHOLD_MINUTES,
                    "minutes_since_last": 20,
                    "causes": [{
                        "kind": "port_conflict",
                        "detail": health.no_events.as_ref().unwrap().causes[0].detail,
                    }],
                },
            })
        );

        // The conflicting and error variants carry their payloads.
        let conflicting = serde_json::to_value(ConfigState::Conflicting {
            installed: true,
            conflicts: vec![],
        })
        .unwrap();
        assert_eq!(
            conflicting,
            json!({"state": "conflicting", "installed": true, "conflicts": []})
        );
    }
}
