//! Uninstall flow (task 2.4).
//!
//! Reverses everything the app set up, in this order:
//!
//! 1. **settings.json**: strict unmerge via [`crate::settings_merge`] -
//!    removes only app-owned env keys (and only at the exact app value) and
//!    the app's `SessionStart` hook entry, with a timestamped backup before
//!    the write. A failure here aborts the whole uninstall before anything
//!    else is touched. After this, newly started Claude Code sessions have
//!    no exporter config and no app hook: they export nothing and run
//!    nothing that could log a hook error.
//! 2. **LaunchAgent**: removed via the autostart plugin. Best-effort once
//!    the unmerge succeeded; a failure is reported, never hidden.
//! 3. **Database**: deleted only when the user explicitly opted in
//!    (`delete_database: true`). The open connection keeps writing to the
//!    unlinked inode until the app exits (harmless); the done screen tells
//!    the user to quit the app to finish.
//!
//! Deliberately **not** removed: settings.json backups (kept so the user
//! can restore any earlier state), every non-app byte of settings.json, and
//! the app bundle itself (the user drags it to the Trash). The confirmation
//! UI in `src/routes/settings/+page.svelte` lists exactly these will/won't
//! items, driven by [`uninstall_status`].

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Manager;

use crate::db;
use crate::onboarding::{backup_dir, diff_lines, render, settings_path, DiffLine};
use crate::settings_merge::{
    apply_unmerge, describe_settings_error, is_installed, read_settings, unmerge_file,
};

/// Everything the confirmation dialog needs to state exactly what will and
/// won't be removed. Read-only.
#[derive(Debug, Clone, Serialize)]
pub struct UninstallStatus {
    /// The app's config is fully present in settings.json.
    pub installed: bool,
    /// Whether the unmerge would change settings.json at all (`false` on a
    /// never-installed or already-uninstalled machine).
    pub settings_changed: bool,
    /// Display path of the settings file.
    pub settings_path: String,
    /// Whether the LaunchAgent is currently registered (read live).
    pub autostart_enabled: bool,
    /// Display path of the database file.
    pub database_path: String,
    /// Whether the database file exists on disk.
    pub database_exists: bool,
    /// Total on-disk size of the database including WAL/shm sidecars.
    pub database_size_bytes: u64,
    /// Where settings.json backups live; never deleted by uninstall.
    pub backups_dir: String,
    /// Line diff of settings.json before → after the unmerge, so the
    /// confirmation shows the literal removal.
    pub diff: Vec<DiffLine>,
}

/// What [`uninstall_apply`] returns to the done screen: per-step results,
/// never a partial mystery.
#[derive(Debug, Clone, Serialize)]
pub struct UninstallOutcome {
    /// Whether settings.json was rewritten (already-clean files are not).
    pub settings_changed: bool,
    /// Backup written before the unmerge, when one was taken.
    pub backup_path: Option<PathBuf>,
    /// LaunchAgent state after the disable attempt (`false` = removed).
    pub autostart_enabled: bool,
    /// Why the LaunchAgent could not be removed, when it couldn't.
    pub autostart_note: Option<String>,
    /// Whether any database file was deleted (always `false` without the
    /// opt-in).
    pub database_deleted: bool,
    /// Why the database was not (fully) deleted, when opted in and failed.
    pub database_note: Option<String>,
}

/// Compute the confirmation-dialog state. Read-only; pure path-in so tests
/// run against temp dirs.
pub fn compute_status(
    settings_path: &Path,
    db_path: &Path,
    backups_dir: &Path,
    autostart_enabled: bool,
) -> Result<UninstallStatus, String> {
    let current =
        read_settings(settings_path).map_err(|err| describe_settings_error(&err, settings_path))?;
    let unmerged = apply_unmerge(&current);

    let before = render(&current);
    let after = render(&unmerged);
    let diff = diff_lines(&before, &after);

    Ok(UninstallStatus {
        installed: is_installed(&current),
        settings_changed: unmerged != current,
        settings_path: settings_path.display().to_string(),
        autostart_enabled,
        database_path: db_path.display().to_string(),
        database_exists: db_path.exists(),
        database_size_bytes: database_size_bytes(db_path),
        backups_dir: backups_dir.display().to_string(),
        diff,
    })
}

/// The database file plus its SQLite sidecars (`-wal`, `-shm`).
fn database_files(db_path: &Path) -> [PathBuf; 3] {
    let with_suffix = |suffix: &str| {
        let mut name = db_path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [
        db_path.to_path_buf(),
        with_suffix("-wal"),
        with_suffix("-shm"),
    ]
}

/// Total on-disk size of the database including WAL/shm sidecars.
pub fn database_size_bytes(db_path: &Path) -> u64 {
    database_files(db_path)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

/// Delete the database file and its sidecars. Idempotent: missing files are
/// fine. Returns whether anything was actually deleted. Unlinking while the
/// app's connection is open is safe on macOS (writes keep going to the
/// unlinked inode and vanish when the app exits); callers hold the DB mutex
/// so no write interleaves with the unlink itself.
pub fn delete_database_files(db_path: &Path) -> std::io::Result<bool> {
    let mut deleted = false;
    for path in database_files(db_path) {
        match std::fs::remove_file(&path) {
            Ok(()) => deleted = true,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(deleted)
}

/// Resolve the database file the app opened at startup
/// (`db::Db::open_in_dir(app_data_dir)`).
fn database_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(db::DB_FILE_NAME))
        .map_err(|err| format!("cannot resolve app data directory: {err}"))
}

/// Frontend query: everything the confirmation dialog states. Read-only.
#[tauri::command]
pub fn uninstall_status(app: tauri::AppHandle) -> Result<UninstallStatus, String> {
    let autostart = crate::autostart::current_status(&app)?;
    compute_status(
        &settings_path(&app)?,
        &database_path(&app)?,
        &backup_dir(&app)?,
        autostart.enabled,
    )
}

/// Frontend action: run the uninstall after the confirmation dialog.
///
/// The settings.json unmerge is the gate: an error there (malformed file,
/// IO) aborts with nothing touched. LaunchAgent removal and the opted-in
/// database deletion then run best-effort, each reporting its own result.
#[tauri::command]
pub fn uninstall_apply(
    app: tauri::AppHandle,
    delete_database: bool,
) -> Result<UninstallOutcome, String> {
    let settings = settings_path(&app)?;
    let outcome = unmerge_file(&settings, &backup_dir(&app)?)
        .map_err(|err| describe_settings_error(&err, &settings))?;

    // LaunchAgent removal: idempotent no-op when not registered. On failure
    // re-read the real state so the done screen never claims a removal that
    // did not happen.
    let (autostart_enabled, autostart_note) = match crate::autostart::set_enabled(&app, false) {
        Ok(status) => (status.enabled, None),
        Err(note) => {
            let enabled = crate::autostart::current_status(&app)
                .map(|status| status.enabled)
                .unwrap_or(true);
            (enabled, Some(note))
        }
    };

    let (database_deleted, database_note) = if delete_database {
        let db_path = database_path(&app)?;
        let state = app.state::<db::DbState>();
        // Hold the DB mutex during the unlink so no write interleaves; a
        // poisoned lock is irrelevant here (we are deleting the file).
        let _guard = match state.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match delete_database_files(&db_path) {
            Ok(deleted) => (
                deleted,
                (!deleted).then(|| "no database files found to delete".to_string()),
            ),
            Err(err) => (false, Some(format!("could not delete database: {err}"))),
        }
    } else {
        (false, None)
    };

    Ok(UninstallOutcome {
        settings_changed: outcome.changed,
        backup_path: outcome.backup_path,
        autostart_enabled,
        autostart_note,
        database_deleted,
        database_note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_merge::{merge_file, APP_ENV};
    use std::path::PathBuf;
    use tempfile::TempDir;

    const REALWORLD: &str = include_str!("../tests/fixtures/settings/realworld.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/settings/malformed.json");

    struct Setup {
        dir: TempDir,
        settings: PathBuf,
        backups: PathBuf,
        db_path: PathBuf,
    }

    fn setup(contents: Option<&str>) -> Setup {
        let dir = TempDir::new().expect("tempdir");
        let settings = dir.path().join("settings.json");
        if let Some(contents) = contents {
            std::fs::write(&settings, contents).expect("write fixture");
        }
        let backups = dir.path().join("backups");
        let db_path = dir.path().join(db::DB_FILE_NAME);
        Setup {
            dir,
            settings,
            backups,
            db_path,
        }
    }

    fn status(s: &Setup) -> UninstallStatus {
        compute_status(&s.settings, &s.db_path, &s.backups, false).expect("status")
    }

    #[test]
    fn installed_machine_previews_full_removal() {
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("install");

        let status = status(&s);
        assert!(status.installed);
        assert!(status.settings_changed);

        // Every app env key and the hook show up as removed lines...
        for (key, _) in APP_ENV {
            assert!(
                status
                    .diff
                    .iter()
                    .any(|line| line.kind == "remove" && line.text.contains(key)),
                "diff does not remove {key}"
            );
        }
        assert!(status
            .diff
            .iter()
            .any(|line| line.kind == "remove" && line.text.contains("SessionStart")));

        // ...and no user content is removed: the previewed end state is
        // semantically identical to the pre-install file (the real
        // guarantee; line-level diff noise is just punctuation reflow).
        let merged = read_settings(&s.settings).expect("read merged");
        let previewed_after = serde_json::Value::Object(apply_unmerge(&merged));
        let original: serde_json::Value = serde_json::from_str(REALWORLD).unwrap();
        assert_eq!(previewed_after, original);

        // Spot-check user content survives on the diff's after side.
        assert!(status.after_contains("ccstatusline"));
        assert!(status.after_contains("spinnerVerbs"));

        // Status is read-only.
        let after = std::fs::read_to_string(&s.settings).unwrap();
        assert!(after.contains("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"));
    }

    #[test]
    fn clean_machine_has_nothing_to_remove() {
        let s = setup(Some(REALWORLD));
        let status = status(&s);
        assert!(!status.installed);
        assert!(!status.settings_changed);
        assert!(status.diff.iter().all(|line| line.kind == "context"));
        assert!(!status.database_exists);
        assert_eq!(status.database_size_bytes, 0);
    }

    #[test]
    fn missing_settings_file_is_a_noop_preview() {
        let s = setup(None);
        let status = status(&s);
        assert!(!status.installed);
        assert!(!status.settings_changed);
    }

    #[test]
    fn user_edited_env_key_is_kept() {
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("install");

        // User takes ownership of one app key by editing its value.
        let raw = std::fs::read_to_string(&s.settings).unwrap();
        let edited = raw.replace("\"otlp\"", "\"console\"");
        assert_ne!(raw, edited, "fixture edit must apply");
        std::fs::write(&s.settings, edited).unwrap();

        let status = status(&s);
        assert!(status.settings_changed, "other app keys still removable");
        // The edited key survives the unmerge with the user's value (the
        // diff may reflow its trailing comma, so check the after side).
        assert!(status.after_contains("OTEL_LOGS_EXPORTER"));
        assert!(status.after_contains("\"console\""));
        // The other app keys still go.
        assert!(!status.after_contains("CLAUDE_CODE_ENABLE_TELEMETRY"));
        assert!(!status.after_contains("SessionStart"));
    }

    impl UninstallStatus {
        fn after_contains(&self, needle: &str) -> bool {
            self.diff
                .iter()
                .filter(|line| line.kind != "remove")
                .any(|line| line.text.contains(needle))
        }
    }

    #[test]
    fn malformed_settings_error_before_anything_else() {
        let s = setup(Some(MALFORMED));
        let err =
            compute_status(&s.settings, &s.db_path, &s.backups, false).expect_err("must error");
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn uninstall_keeps_backups_dir() {
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("install");
        unmerge_file(&s.settings, &s.backups).expect("uninstall");

        // Both the install and the uninstall backup survive, and the file is
        // back to its pre-install content (modulo pretty-print formatting).
        let backups: Vec<_> = std::fs::read_dir(&s.backups).unwrap().collect();
        assert_eq!(backups.len(), 2);
        let restored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&s.settings).unwrap()).unwrap();
        let original: serde_json::Value = serde_json::from_str(REALWORLD).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn delete_database_files_removes_db_and_sidecars_while_open() {
        let s = setup(None);
        // Real open connection with a WAL write, like the running app.
        let database = db::Db::open_in_dir(s.dir.path()).expect("open db");
        database
            .conn()
            .execute(
                "INSERT INTO sessions (session_id, first_seen_ms) VALUES ('s1', 1)",
                [],
            )
            .unwrap();
        assert!(s.db_path.exists());
        let wal = PathBuf::from(format!("{}-wal", s.db_path.display()));
        assert!(wal.exists(), "WAL sidecar expected after a write");
        assert!(database_size_bytes(&s.db_path) > 0);

        let deleted = delete_database_files(&s.db_path).expect("delete");
        assert!(deleted);
        for path in database_files(&s.db_path) {
            assert!(!path.exists(), "{} still exists", path.display());
        }
        assert_eq!(database_size_bytes(&s.db_path), 0);
    }

    #[test]
    fn delete_database_files_is_idempotent_on_missing_files() {
        let s = setup(None);
        let deleted = delete_database_files(&s.db_path).expect("delete");
        assert!(!deleted);
    }

    #[test]
    fn status_and_outcome_serialize_for_frontend() {
        let s = setup(None);
        let value = serde_json::to_value(status(&s)).unwrap();
        for field in [
            "installed",
            "settings_changed",
            "settings_path",
            "autostart_enabled",
            "database_path",
            "database_exists",
            "database_size_bytes",
            "backups_dir",
            "diff",
        ] {
            assert!(value.get(field).is_some(), "missing field {field}");
        }

        let outcome = UninstallOutcome {
            settings_changed: true,
            backup_path: Some(PathBuf::from("/tmp/b.json")),
            autostart_enabled: false,
            autostart_note: None,
            database_deleted: true,
            database_note: None,
        };
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({
                "settings_changed": true,
                "backup_path": "/tmp/b.json",
                "autostart_enabled": false,
                "autostart_note": null,
                "database_deleted": true,
                "database_note": null,
            })
        );
    }
}
