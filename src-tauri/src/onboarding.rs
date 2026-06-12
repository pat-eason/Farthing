//! Onboarding flow backend (task 2.2).
//!
//! Thin, UI-facing layer over the merge engine ([`crate::settings_merge`]):
//! computes the exact before/after state of `~/.claude/settings.json`, a
//! line diff for the preview screen, and the conflict list; applies the
//! merge only on explicit confirmation. Nothing here writes without the
//! frontend calling [`onboarding_apply`], and a merge that would collide
//! with pre-existing telemetry config is refused unless the caller passes
//! `acknowledge_conflicts: true` (the conflict screen's explicit choice).
//!
//! The pure functions ([`compute_status`], [`apply`]) take paths so tests
//! run against temp dirs; the `#[tauri::command]` wrappers resolve the real
//! `~/.claude/settings.json` and the app-data backups dir. For development
//! the settings path can be overridden with the
//! `FARTHING_SETTINGS_PATH` env var so the flow can be exercised
//! against a scratch file instead of the real one.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};
use similar::{ChangeTag, TextDiff};
use tauri::Manager;

use crate::settings_merge::{
    apply_merge, describe_settings_error, detect_conflicts, is_installed, merge_file,
    read_settings, ApplyOutcome, Conflict,
};

/// Dev/test override for the settings file location. Never set in
/// production; the real path is `~/.claude/settings.json`.
pub const SETTINGS_PATH_ENV: &str = "FARTHING_SETTINGS_PATH";

/// One line of the before/after diff shown on the preview screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// `"context"`, `"add"`, or `"remove"`.
    pub kind: &'static str,
    /// Line text without the trailing newline.
    pub text: String,
}

/// What [`onboarding_apply`] returns to the done screen: the merge outcome
/// plus the best-effort autostart registration (task 2.3). Autostart runs
/// only after a successful merge and never fails it.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardingApplyOutcome {
    /// Whether settings.json was rewritten (see [`ApplyOutcome::changed`]).
    pub changed: bool,
    /// Backup written before the rewrite, when one was taken.
    pub backup_path: Option<std::path::PathBuf>,
    /// Whether the login item got registered.
    pub autostart_enabled: bool,
    /// Why autostart is not enabled (dev build, plugin error), shown as an
    /// informational note; the settings view has the toggle to retry.
    pub autostart_note: Option<String>,
}

/// Everything the onboarding UI needs to render the right screen.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardingStatus {
    /// All app env keys and the SessionStart hook are present: the
    /// "already configured" state.
    pub installed: bool,
    /// Whether applying the merge would change the file at all. `false`
    /// makes re-running onboarding a guaranteed no-op.
    pub changed: bool,
    /// Pre-existing telemetry config the merge would interact with.
    /// Non-empty requires the conflict screen's explicit choice.
    pub conflicts: Vec<Conflict>,
    /// Display path of the settings file.
    pub settings_path: String,
    /// The settings file as it is now (pretty-printed; this is also the
    /// formatting the merge writes, so `after` is byte-exact future content).
    pub before: String,
    /// The settings file as it would be after the merge.
    pub after: String,
    /// Line diff of `before` → `after`.
    pub diff: Vec<DiffLine>,
}

/// Read the settings file and compute the full preview: install state,
/// conflicts, rendered before/after, and the line diff. Read-only.
///
/// Errors (malformed JSON, unexpected shapes, IO) come back as display
/// strings for the UI; the merge engine guarantees those cases never write.
pub fn compute_status(settings_path: &Path) -> Result<OnboardingStatus, String> {
    let current =
        read_settings(settings_path).map_err(|err| describe_settings_error(&err, settings_path))?;
    let merged =
        apply_merge(&current).map_err(|err| describe_settings_error(&err, settings_path))?;

    let before = render(&current);
    let after = render(&merged);
    let diff = diff_lines(&before, &after);

    Ok(OnboardingStatus {
        installed: is_installed(&current),
        changed: merged != current,
        conflicts: detect_conflicts(&current),
        settings_path: settings_path.display().to_string(),
        before,
        after,
        diff,
    })
}

/// Apply the merge after user confirmation. Re-checks conflicts at apply
/// time (the file may have changed since the preview): when conflicts exist
/// and `acknowledge_conflicts` is `false`, refuses without touching the
/// file. The conflict screen's explicit "overwrite" choice passes `true`.
pub fn apply(
    settings_path: &Path,
    backup_dir: &Path,
    acknowledge_conflicts: bool,
) -> Result<ApplyOutcome, String> {
    let current =
        read_settings(settings_path).map_err(|err| describe_settings_error(&err, settings_path))?;
    let conflicts = detect_conflicts(&current);
    if !conflicts.is_empty() && !acknowledge_conflicts {
        return Err(format!(
            "settings.json has {} pre-existing telemetry setting(s); explicit confirmation required",
            conflicts.len()
        ));
    }
    merge_file(settings_path, backup_dir)
        .map_err(|err| describe_settings_error(&err, settings_path))
}

/// Pretty-print a settings map exactly as the merge engine writes it
/// (2-space indent, trailing newline), so the preview's `after` matches the
/// future file bytes. Shared with the uninstall flow (task 2.4).
pub(crate) fn render(map: &Map<String, Value>) -> String {
    let mut rendered = serde_json::to_string_pretty(&Value::Object(map.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    rendered.push('\n');
    rendered
}

/// Line-based diff for the preview screen. Shared with the uninstall flow.
pub(crate) fn diff_lines(before: &str, after: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(before, after)
        .iter_all_changes()
        .map(|change| DiffLine {
            kind: match change.tag() {
                ChangeTag::Equal => "context",
                ChangeTag::Insert => "add",
                ChangeTag::Delete => "remove",
            },
            text: change.value().trim_end_matches('\n').to_string(),
        })
        .collect()
}

/// Resolve the real settings file: the env override when set (dev/testing),
/// otherwise `~/.claude/settings.json`. Shared with the uninstall flow and
/// the health view (generic over the runtime so MockRuntime tests work).
pub(crate) fn settings_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var(SETTINGS_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    app.path()
        .home_dir()
        .map(|home| home.join(".claude").join("settings.json"))
        .map_err(|err| format!("cannot resolve home directory: {err}"))
}

/// Backups live next to the database in the app-data dir, never inside
/// `~/.claude` (uninstall must not leave litter there). Shared with the
/// uninstall flow, which writes one last backup before unmerging and
/// deliberately never deletes this directory.
pub(crate) fn backup_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("backups"))
        .map_err(|err| format!("cannot resolve app data directory: {err}"))
}

/// Frontend query: current config state + merge preview. Read-only.
#[tauri::command]
pub fn onboarding_status(app: tauri::AppHandle) -> Result<OnboardingStatus, String> {
    compute_status(&settings_path(&app)?)
}

/// Frontend action: apply the merge (backup first, atomic write). Gated on
/// the user-confirmed diff; `acknowledge_conflicts` must be `true` when the
/// preview reported conflicts (the conflict screen's explicit choice).
///
/// After a successful merge, registers the app as a login item (PRD: the
/// receiver must always be up). Best-effort: an autostart failure (or the
/// dev-build guard) is reported in the outcome, never as an error.
#[tauri::command]
pub fn onboarding_apply(
    app: tauri::AppHandle,
    acknowledge_conflicts: bool,
) -> Result<OnboardingApplyOutcome, String> {
    let outcome = apply(
        &settings_path(&app)?,
        &backup_dir(&app)?,
        acknowledge_conflicts,
    )?;
    let autostart = crate::autostart::enable_after_onboarding(&app);
    Ok(OnboardingApplyOutcome {
        changed: outcome.changed,
        backup_path: outcome.backup_path,
        autostart_enabled: autostart.enabled,
        autostart_note: autostart.note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    const REALWORLD: &str = include_str!("../tests/fixtures/settings/realworld.json");
    const PREEXISTING_ENV: &str = include_str!("../tests/fixtures/settings/preexisting_env.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/settings/malformed.json");

    struct Setup {
        _dir: TempDir,
        settings: PathBuf,
        backups: PathBuf,
    }

    fn setup(contents: Option<&str>) -> Setup {
        let dir = TempDir::new().expect("tempdir");
        let settings = dir.path().join("settings.json");
        if let Some(contents) = contents {
            std::fs::write(&settings, contents).expect("write fixture");
        }
        let backups = dir.path().join("backups");
        Setup {
            _dir: dir,
            settings,
            backups,
        }
    }

    fn backup_count(dir: &Path) -> usize {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries.count(),
            Err(err) if err.kind() == ErrorKind::NotFound => 0,
            Err(err) => panic!("read backups dir: {err}"),
        }
    }

    #[test]
    fn fresh_machine_status_previews_full_install() {
        let s = setup(None);
        let status = compute_status(&s.settings).expect("status");
        assert!(!status.installed);
        assert!(status.changed);
        assert!(status.conflicts.is_empty());
        assert_eq!(status.before, "{}\n");

        // The preview's `after` shows the app endpoint; the diff carries it
        // as an added line and removes nothing (fresh file).
        assert!(status.after.contains("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"));
        assert!(status
            .diff
            .iter()
            .any(|line| line.kind == "add" && line.text.contains("127.0.0.1:43177/v1/logs")));
        assert!(status
            .diff
            .iter()
            .any(|line| line.kind == "add" && line.text.contains("SessionStart")));

        // Status is read-only: nothing was written.
        assert!(!s.settings.exists());
    }

    #[test]
    fn realworld_preview_keeps_user_content_in_both_sides() {
        let s = setup(Some(REALWORLD));
        let status = compute_status(&s.settings).expect("status");
        assert!(!status.installed);
        assert!(status.changed);
        assert!(status.conflicts.is_empty());

        // Every original line survives into `after`: a "removed" line is
        // only ever punctuation reflow around the insertion point (e.g. the
        // previously-last key gaining a trailing comma), so its
        // comma-stripped text must reappear as an added line.
        let added: Vec<String> = status
            .diff
            .iter()
            .filter(|line| line.kind == "add")
            .map(|line| line.text.trim().trim_end_matches(',').to_string())
            .collect();
        for line in status.diff.iter().filter(|line| line.kind == "remove") {
            let bare = line.text.trim().trim_end_matches(',');
            assert!(
                added.iter().any(|add| add == bare),
                "user content removed in preview: {:?}",
                line.text
            );
        }
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), REALWORLD);
    }

    #[test]
    fn preexisting_telemetry_surfaces_conflicts_in_status() {
        let s = setup(Some(PREEXISTING_ENV));
        let status = compute_status(&s.settings).expect("status");
        assert_eq!(status.conflicts.len(), 3);
        // The overwrite conflict appears in the diff as remove+add.
        assert!(status
            .diff
            .iter()
            .any(|line| line.kind == "remove" && line.text.contains("console")));
        assert!(status
            .diff
            .iter()
            .any(|line| line.kind == "add" && line.text.contains("\"otlp\"")));
    }

    #[test]
    fn malformed_settings_error_and_never_written() {
        let s = setup(Some(MALFORMED));
        let err = compute_status(&s.settings).expect_err("must error");
        assert!(err.contains("not valid JSON"), "got: {err}");

        let err = apply(&s.settings, &s.backups, true).expect_err("apply must error too");
        assert!(err.contains("not valid JSON"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), MALFORMED);
        assert_eq!(backup_count(&s.backups), 0);
    }

    #[test]
    fn apply_refuses_unacknowledged_conflicts() {
        let s = setup(Some(PREEXISTING_ENV));
        let err = apply(&s.settings, &s.backups, false).expect_err("must refuse");
        assert!(err.contains("explicit confirmation required"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&s.settings).unwrap(),
            PREEXISTING_ENV,
            "file untouched on refusal"
        );
        assert_eq!(backup_count(&s.backups), 0);
    }

    #[test]
    fn apply_with_acknowledged_conflicts_merges() {
        let s = setup(Some(PREEXISTING_ENV));
        let outcome = apply(&s.settings, &s.backups, true).expect("apply");
        assert!(outcome.changed);
        assert!(outcome.backup_path.is_some());

        let status = compute_status(&s.settings).expect("status");
        assert!(status.installed);
        assert!(!status.changed);
    }

    #[test]
    fn apply_without_conflicts_needs_no_acknowledgement() {
        let s = setup(Some(REALWORLD));
        let outcome = apply(&s.settings, &s.backups, false).expect("apply");
        assert!(outcome.changed);
        assert!(compute_status(&s.settings).expect("status").installed);
    }

    #[test]
    fn rerun_on_configured_machine_is_a_noop() {
        let s = setup(None);
        apply(&s.settings, &s.backups, false).expect("first apply");
        let installed_bytes = std::fs::read_to_string(&s.settings).unwrap();

        // Status reports the already-configured state...
        let status = compute_status(&s.settings).expect("status");
        assert!(status.installed);
        assert!(!status.changed);
        assert!(
            status.diff.iter().all(|line| line.kind == "context"),
            "no-op diff is all context"
        );

        // ...and re-applying changes nothing: no write, no extra backup.
        let outcome = apply(&s.settings, &s.backups, false).expect("re-apply");
        assert!(!outcome.changed);
        assert_eq!(outcome.backup_path, None);
        assert_eq!(
            std::fs::read_to_string(&s.settings).unwrap(),
            installed_bytes
        );
        assert_eq!(
            backup_count(&s.backups),
            0,
            "fresh-file installs never back up"
        );
    }

    #[test]
    fn diff_line_serializes_for_frontend() {
        let line = DiffLine {
            kind: "add",
            text: "  \"foo\": 1".into(),
        };
        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            serde_json::json!({"kind": "add", "text": "  \"foo\": 1"})
        );
    }
}
