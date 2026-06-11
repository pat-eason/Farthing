//! Capture pause/resume (task 4.4).
//!
//! One shared flag decides whether the receiver stores what arrives: while
//! paused, every receiver endpoint keeps returning success but discards the
//! payload (PRD FR-5 — the export side must never see errors, and the
//! paused window is recoverable later via transcript backfill).
//!
//! The flag is persisted in the `meta` table under [`PAUSED_KEY`] so a
//! paused app stays paused across restarts; [`CaptureState::load`] reads it
//! back on startup before the receiver spawns.
//!
//! State changes fan out from [`apply_paused`]:
//! - the persisted flag + the atomic the receiver consults
//! - the tray UI (menu check item + "Paused" badge, `tray::sync_paused_ui`)
//! - the frontend via the [`PAUSED_CHANGED_EVENT`] event (popover badge)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{Emitter, Manager, Runtime};

use crate::db::Db;

/// `meta` key holding the persisted pause flag (`"1"` paused, `"0"` not).
pub const PAUSED_KEY: &str = "capture_paused";

/// Event emitted to the frontend whenever the pause state changes; payload
/// is a [`CaptureStatus`].
pub const PAUSED_CHANGED_EVENT: &str = "capture:paused-changed";

/// Shared pause state: the atomic flag the receiver checks per request plus
/// the database handle used to persist changes. Managed in the Tauri app.
#[derive(Clone)]
pub struct CaptureState {
    paused: Arc<AtomicBool>,
    db: Arc<Mutex<Db>>,
}

/// Pause state for the frontend (`capture_status` command and the
/// [`PAUSED_CHANGED_EVENT`] payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CaptureStatus {
    pub paused: bool,
}

impl CaptureState {
    /// Read the persisted flag (default: not paused) and wrap it for
    /// sharing. A missing or unreadable `meta` row never fails startup.
    pub fn load(db: Arc<Mutex<Db>>) -> Self {
        let paused = read_persisted(&db);
        Self {
            paused: Arc::new(AtomicBool::new(paused)),
            db,
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// The flag the receiver's router state consults on every request.
    pub fn pause_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.paused)
    }

    /// Persist `paused` and flip the shared flag. The write happens first:
    /// if it fails, the in-memory state is left unchanged so UI and disk
    /// never disagree.
    pub fn set_paused(&self, paused: bool) -> Result<(), rusqlite::Error> {
        {
            let db = self.db.lock().expect("db mutex poisoned");
            db.conn().execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                rusqlite::params![PAUSED_KEY, if paused { "1" } else { "0" }],
            )?;
        }
        self.paused.store(paused, Ordering::SeqCst);
        Ok(())
    }

    pub fn status(&self) -> CaptureStatus {
        CaptureStatus {
            paused: self.paused(),
        }
    }
}

fn read_persisted(db: &Mutex<Db>) -> bool {
    let db = db.lock().expect("db mutex poisoned");
    db.conn()
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [PAUSED_KEY],
            |row| row.get::<_, String>(0),
        )
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// Single entry point for every pause-state change (tray menu, popover
/// resume button): persist + flip, then sync the tray UI and notify the
/// frontend. Returns the resulting status.
pub fn apply_paused<R: Runtime>(
    app: &tauri::AppHandle<R>,
    paused: bool,
) -> Result<CaptureStatus, String> {
    let state = app.state::<CaptureState>();
    state
        .set_paused(paused)
        .map_err(|err| format!("cannot persist pause state: {err}"))?;
    let status = state.status();
    // Tray/menu mutations only take effect on the main thread; commands run
    // on the async runtime (live-verified: a set_title there is dropped).
    let ui_app = app.clone();
    let _ = app.run_on_main_thread(move || crate::tray::sync_paused_ui(&ui_app, paused));
    let _ = app.emit(PAUSED_CHANGED_EVENT, status);
    Ok(status)
}

/// Frontend query: current pause state (popover badge on open).
#[tauri::command]
pub fn capture_status(state: tauri::State<'_, CaptureState>) -> CaptureStatus {
    state.status()
}

/// Frontend action: pause or resume capture (popover resume button).
#[tauri::command]
pub fn capture_set_paused<R: Runtime>(
    app: tauri::AppHandle<R>,
    paused: bool,
) -> Result<CaptureStatus, String> {
    apply_paused(&app, paused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db(dir: &tempfile::TempDir) -> Arc<Mutex<Db>> {
        Arc::new(Mutex::new(Db::open_in_dir(dir.path()).unwrap()))
    }

    #[test]
    fn defaults_to_not_paused_on_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let state = CaptureState::load(test_db(&dir));
        assert!(!state.paused());
    }

    #[test]
    fn set_paused_flips_the_shared_flag() {
        let dir = tempfile::tempdir().unwrap();
        let state = CaptureState::load(test_db(&dir));
        let receiver_view = state.pause_flag();

        state.set_paused(true).unwrap();
        assert!(state.paused());
        assert!(receiver_view.load(Ordering::SeqCst), "flag is shared");

        state.set_paused(false).unwrap();
        assert!(!receiver_view.load(Ordering::SeqCst));
    }

    /// The restart criterion: pause, reopen the database file from disk as
    /// a fresh process would, and load the state again.
    #[test]
    fn paused_state_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let state = CaptureState::load(test_db(&dir));
            state.set_paused(true).unwrap();
        }
        let reloaded = CaptureState::load(test_db(&dir));
        assert!(reloaded.paused(), "persisted pause must be read on startup");

        reloaded.set_paused(false).unwrap();
        let reloaded_again = CaptureState::load(test_db(&dir));
        assert!(!reloaded_again.paused(), "resume persists too");
    }

    #[test]
    fn apply_paused_persists_and_reports_via_mock_app() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(CaptureState::load(Arc::clone(&db)));

        let status = apply_paused(app.handle(), true).expect("apply");
        assert_eq!(status, CaptureStatus { paused: true });
        assert!(app.state::<CaptureState>().paused());

        // Persisted: a fresh load from the same database sees it.
        assert!(CaptureState::load(db).paused());

        let status = apply_paused(app.handle(), false).expect("apply");
        assert!(!status.paused);
    }

    #[test]
    fn status_serializes_for_frontend() {
        assert_eq!(
            serde_json::to_value(CaptureStatus { paused: true }).unwrap(),
            serde_json::json!({"paused": true})
        );
    }
}
