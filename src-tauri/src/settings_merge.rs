//! settings.json merge engine (task 2.1).
//!
//! Safe, reversible modification of the user's `~/.claude/settings.json`:
//! a deep-merge that adds exactly [`APP_ENV`] (5 env keys) and one
//! `SessionStart` hook entry, a timestamped backup before any write, and a
//! strict unmerge that removes only app-owned content. This is the
//! highest-blast-radius code in the app: a bug here corrupts the user's
//! Claude Code configuration, so every behavior is fixture-tested and the
//! engine **never writes** when it cannot fully parse the existing file.
//!
//! The engine is pure path-in/path-out: it does not know where the real
//! settings file or backup directory live. The onboarding flow (task 2.2)
//! resolves `~/.claude/settings.json` and the app-data backups dir and is
//! responsible for gating [`merge_file`] behind a user-confirmed diff and
//! [`detect_conflicts`].
//!
//! # What the merge writes
//!
//! The env block is the e2e-verified minimal config from
//! `docs/notes/otel-schema.md` (Claude Code v2.1.173 honors only the
//! signal-specific OTLP exporter vars for logs) plus the generic
//! `OTEL_EXPORTER_OTLP_PROTOCOL` for tolerance of Claude Code versions that
//! read the generic var, 5 keys total. The hook entry is the exact curl
//! command verified in task 1.6, made fail-silent (PRD: the hook must never
//! slow down or break Claude Code).
//!
//! # Ownership rules
//!
//! - **Env**: the app owns the 5 [`APP_ENV`] keys. Unmerge removes a key
//!   only when its value is still exactly what the app wrote; if the user
//!   edited it, they took ownership and the key is left alone.
//! - **Hooks**: any `SessionStart` command containing
//!   [`APP_HOOK_MARKER`] (the app's literal `/session` endpoint) is
//!   app-owned. Everything else - user hook groups, other hook events,
//!   every other byte of the file - is preserved verbatim (key order is
//!   kept via serde_json's `preserve_order`; whitespace is normalized to
//!   2-space pretty-printing on write).

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Map, Value};

/// The exact env keys the app owns in `settings.json`.
///
/// First four are the verified-minimal block from `docs/notes/otel-schema.md`
/// (the signal-specific pair is required; the generic endpoint is ignored by
/// Claude Code v2.1.173). The generic `OTEL_EXPORTER_OTLP_PROTOCOL` is
/// belt-and-suspenders for versions that fall back to the generic var; the
/// generic `OTEL_EXPORTER_OTLP_ENDPOINT` is deliberately **not** set so the
/// app never redirects other OTel signals the user may export elsewhere.
pub const APP_ENV: [(&str, &str); 5] = [
    ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
    ("OTEL_LOGS_EXPORTER", "otlp"),
    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
    ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json"),
    (
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "http://127.0.0.1:43177/v1/logs",
    ),
];

/// Substring identifying an app-owned `SessionStart` hook command. Matching
/// on the literal endpoint (rather than the full command string) keeps
/// unmerge working even if a future app version tweaks curl flags.
pub const APP_HOOK_MARKER: &str = "http://127.0.0.1:43177/session";

/// The `SessionStart` hook command: POSTs the hook's stdin JSON
/// (`session_id` + `cwd`) to the receiver's `/session` endpoint
/// (`src/session.rs`). Verified live in task 1.6; the trailing redirect +
/// `|| true` make it fail-silent when the app isn't running.
pub const SESSION_HOOK_COMMAND: &str = "curl -s -m 2 -X POST -H 'Content-Type: application/json' \
     --data-binary @- http://127.0.0.1:43177/session >/dev/null 2>&1 || true";

/// Errors from reading, merging, or writing settings.json. Any error aborts
/// before a write: the engine never persists a partial or lossy result.
#[derive(Debug)]
pub enum SettingsError {
    /// Filesystem failure (read, write, backup copy, rename).
    Io(std::io::Error),
    /// The settings file exists but is not valid JSON. Abort, never write.
    Malformed(serde_json::Error),
    /// The file parses but a structure the merge must edit has an
    /// unexpected type (root not an object, `env` not an object, ...).
    /// Abort, never write.
    UnexpectedShape(&'static str),
    /// Backup file to restore from does not exist.
    BackupMissing(PathBuf),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::Io(err) => write!(f, "settings.json io error: {err}"),
            SettingsError::Malformed(err) => {
                write!(
                    f,
                    "settings.json is not valid JSON (refusing to write): {err}"
                )
            }
            SettingsError::UnexpectedShape(what) => write!(
                f,
                "settings.json has an unexpected shape at `{what}` (refusing to write)"
            ),
            SettingsError::BackupMissing(path) => {
                write!(f, "backup file not found: {}", path.display())
            }
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<std::io::Error> for SettingsError {
    fn from(err: std::io::Error) -> Self {
        SettingsError::Io(err)
    }
}

/// Render a [`SettingsError`] for the UI with the file path and, where the
/// error kind admits one, a concrete remediation hint. Shared by the
/// onboarding, uninstall, and health surfaces (task 6.4) so every
/// settings.json failure names the file and says what to do about it.
pub fn describe_settings_error(err: &SettingsError, path: &Path) -> String {
    let base = format!("{err} (file: {})", path.display());
    match err {
        SettingsError::Io(io) if io.kind() == ErrorKind::PermissionDenied => format!(
            "{base}. This app does not have permission to access the file; check its \
             permissions (e.g. `chmod u+rw` it) and that the containing folder is accessible."
        ),
        SettingsError::Io(io) if io.kind() == ErrorKind::StorageFull => format!(
            "{base}. The disk is full; free up space and try again. The previous file \
             contents are intact (writes are atomic and happen after a backup)."
        ),
        SettingsError::Malformed(_) => format!(
            "{base}. Fix the JSON syntax (or restore one of the timestamped backups) and \
             try again; the file has not been modified."
        ),
        _ => base,
    }
}

/// Result of a [`merge_file`] / [`unmerge_file`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyOutcome {
    /// Whether the file was rewritten. `false` means it was already in the
    /// target state; no backup was taken and no bytes were touched.
    pub changed: bool,
    /// Backup written before the rewrite (`None` when `changed` is `false`
    /// or there was no pre-existing file to back up).
    pub backup_path: Option<PathBuf>,
}

/// A pre-existing telemetry setting the merge would interact with. The
/// onboarding flow (2.2) must surface these and require an explicit user
/// choice before calling [`merge_file`] - never silently overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// The env key in conflict.
    pub key: String,
    /// The user's current value.
    pub existing: Value,
    /// What the app wants to write (`None` for telemetry keys the app does
    /// not own but which suggest an existing OTel setup, e.g. a generic
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` pointing at another collector).
    pub proposed: Option<String>,
}

/// Reads and parses a settings file. A missing or empty/whitespace-only
/// file is an empty object (Claude Code treats both as "no settings");
/// malformed JSON or a non-object root is an error so callers abort before
/// any write.
pub fn read_settings(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Map::new()),
        Err(err) => return Err(SettingsError::Io(err)),
    };
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&raw).map_err(SettingsError::Malformed)? {
        Value::Object(map) => Ok(map),
        _ => Err(SettingsError::UnexpectedShape("root")),
    }
}

/// Pure merge: returns a copy of `current` with exactly the [`APP_ENV`]
/// keys set and one app `SessionStart` hook entry appended (idempotent: an
/// already-present app hook is not duplicated). Everything else is carried
/// over untouched.
///
/// Overwrites an app-owned env key that holds a different value; callers
/// must gate on [`detect_conflicts`] first (task 2.2's conflict screen).
pub fn apply_merge(current: &Map<String, Value>) -> Result<Map<String, Value>, SettingsError> {
    let mut out = current.clone();

    let env = out
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(SettingsError::UnexpectedShape("env"))?;
    for (key, value) in APP_ENV {
        env.insert(key.to_string(), Value::String(value.to_string()));
    }

    let hooks = out
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(SettingsError::UnexpectedShape("hooks"))?;
    let session_start = hooks
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or(SettingsError::UnexpectedShape("hooks.SessionStart"))?;
    if !session_start.iter().any(group_has_app_hook) {
        session_start.push(app_hook_group());
    }

    Ok(out)
}

/// Pure strict unmerge: returns a copy of `current` with only app-owned
/// content removed.
///
/// - Env keys are removed only when still holding the exact app value
///   (a user-edited value means the user took ownership; left alone).
/// - `SessionStart` inner hooks whose command contains [`APP_HOOK_MARKER`]
///   are removed; a hook group is dropped only when this removal emptied it.
/// - Containers (`env`, `hooks`, `SessionStart`) are removed when left
///   empty; user hook groups, other hook events, and all other keys are
///   untouched.
///
/// Infallible by design: unexpected shapes simply contain nothing app-owned
/// and are preserved as-is.
pub fn apply_unmerge(current: &Map<String, Value>) -> Map<String, Value> {
    let mut out = current.clone();

    if let Some(Value::Object(env)) = out.get_mut("env") {
        for (key, value) in APP_ENV {
            if env.get(key).and_then(Value::as_str) == Some(value) {
                env.remove(key);
            }
        }
    }
    if matches!(out.get("env"), Some(Value::Object(env)) if env.is_empty()) {
        out.remove("env");
    }

    if let Some(Value::Object(hooks)) = out.get_mut("hooks") {
        if let Some(Value::Array(groups)) = hooks.get_mut("SessionStart") {
            groups.retain_mut(|group| {
                let Some(group) = group.as_object_mut() else {
                    return true;
                };
                let Some(Value::Array(inner)) = group.get_mut("hooks") else {
                    return true;
                };
                let had_hooks = !inner.is_empty();
                inner.retain(|hook| !is_app_hook(hook));
                // Drop the group only when removing app hooks emptied it; a
                // group that was already empty (or still has user hooks)
                // is user content.
                !(had_hooks && inner.is_empty())
            });
        }
        if matches!(hooks.get("SessionStart"), Some(Value::Array(groups)) if groups.is_empty()) {
            hooks.remove("SessionStart");
        }
    }
    if matches!(out.get("hooks"), Some(Value::Object(hooks)) if hooks.is_empty()) {
        out.remove("hooks");
    }

    out
}

/// Whether the app's config is fully installed: all 5 [`APP_ENV`] keys at
/// the app values and the `SessionStart` hook present.
pub fn is_installed(current: &Map<String, Value>) -> bool {
    let env_ok = matches!(
        current.get("env"),
        Some(Value::Object(env)) if APP_ENV
            .iter()
            .all(|(key, value)| env.get(*key).and_then(Value::as_str) == Some(*value))
    );
    let hook_ok = current
        .get("hooks")
        .and_then(|hooks| hooks.get("SessionStart"))
        .and_then(Value::as_array)
        .is_some_and(|groups| groups.iter().any(group_has_app_hook));
    env_ok && hook_ok
}

/// Pre-existing telemetry config the merge would collide with (PRD: surface
/// and require an explicit choice, never silently overwrite):
///
/// - an app-owned env key holding a **different** value (`proposed` is the
///   app value that would overwrite it), or
/// - any other `OTEL_*` / `CLAUDE_CODE_*TELEMETRY*` env key, which signals
///   an existing OTel setup pointing elsewhere (`proposed` is `None`; the
///   merge leaves these untouched but they may interact with the app's
///   exporter config).
///
/// App-owned keys already at the app value are not conflicts. Pre-existing
/// user `SessionStart` hooks are not conflicts either: the merge is purely
/// additive there.
pub fn detect_conflicts(current: &Map<String, Value>) -> Vec<Conflict> {
    let Some(Value::Object(env)) = current.get("env") else {
        return Vec::new();
    };
    let mut conflicts = Vec::new();
    for (key, existing) in env {
        match APP_ENV.iter().find(|(app_key, _)| app_key == key) {
            Some((_, app_value)) if existing.as_str() != Some(*app_value) => {
                conflicts.push(Conflict {
                    key: key.clone(),
                    existing: existing.clone(),
                    proposed: Some((*app_value).to_string()),
                });
            }
            // App-owned key already at the app value: not a conflict.
            Some(_) => {}
            None if key.starts_with("OTEL_")
                || (key.starts_with("CLAUDE_CODE_") && key.contains("TELEMETRY")) =>
            {
                conflicts.push(Conflict {
                    key: key.clone(),
                    existing: existing.clone(),
                    proposed: None,
                });
            }
            None => {}
        }
    }
    conflicts
}

/// Copies the current settings file into `backup_dir` as
/// `settings-<UTC timestamp>.json` before a rewrite. Returns `None` when
/// there is nothing to back up (no pre-existing file). Never overwrites an
/// existing backup: same-instant collisions get a `-<n>` suffix.
pub fn write_backup(
    settings_path: &Path,
    backup_dir: &Path,
) -> Result<Option<PathBuf>, SettingsError> {
    let raw = match std::fs::read(settings_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(SettingsError::Io(err)),
    };
    std::fs::create_dir_all(backup_dir)?;
    let stamp = utc_timestamp();
    let mut backup_path = backup_dir.join(format!("settings-{stamp}.json"));
    let mut counter = 1;
    while backup_path.exists() {
        backup_path = backup_dir.join(format!("settings-{stamp}-{counter}.json"));
        counter += 1;
    }
    write_atomic(&backup_path, &raw)?;
    Ok(Some(backup_path))
}

/// Restores `settings_path` to the exact bytes of a backup written by
/// [`write_backup`].
pub fn restore_from_backup(backup_path: &Path, settings_path: &Path) -> Result<(), SettingsError> {
    let raw = match std::fs::read(backup_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(SettingsError::BackupMissing(backup_path.to_path_buf()))
        }
        Err(err) => return Err(SettingsError::Io(err)),
    };
    write_atomic(settings_path, &raw)?;
    Ok(())
}

/// Read → [`apply_merge`] → backup → atomic write. Skips the backup and the
/// write entirely when the file is already in the merged state. Any parse
/// or shape error aborts before bytes are touched.
pub fn merge_file(settings_path: &Path, backup_dir: &Path) -> Result<ApplyOutcome, SettingsError> {
    let current = read_settings(settings_path)?;
    let merged = apply_merge(&current)?;
    write_if_changed(settings_path, backup_dir, &current, merged)
}

/// Read → [`apply_unmerge`] → backup → atomic write. Skips the backup and
/// the write when there is nothing app-owned to remove (including a missing
/// file). Malformed JSON aborts: unmerge also never writes what it cannot
/// fully parse.
pub fn unmerge_file(
    settings_path: &Path,
    backup_dir: &Path,
) -> Result<ApplyOutcome, SettingsError> {
    let current = read_settings(settings_path)?;
    let unmerged = apply_unmerge(&current);
    write_if_changed(settings_path, backup_dir, &current, unmerged)
}

fn write_if_changed(
    settings_path: &Path,
    backup_dir: &Path,
    current: &Map<String, Value>,
    target: Map<String, Value>,
) -> Result<ApplyOutcome, SettingsError> {
    if &target == current {
        return Ok(ApplyOutcome {
            changed: false,
            backup_path: None,
        });
    }
    let backup_path = write_backup(settings_path, backup_dir)?;
    let mut rendered =
        serde_json::to_string_pretty(&Value::Object(target)).map_err(SettingsError::Malformed)?;
    rendered.push('\n');
    write_atomic(settings_path, rendered.as_bytes())?;
    Ok(ApplyOutcome {
        changed: true,
        backup_path,
    })
}

/// The hook entry the merge appends to `hooks.SessionStart`. No `matcher`:
/// the session→cwd mapping wants every start source (`startup`, `resume`,
/// `clear`, `compact`).
fn app_hook_group() -> Value {
    json!({
        "hooks": [
            { "type": "command", "command": SESSION_HOOK_COMMAND }
        ]
    })
}

/// Whether an inner hook object (`{"type": "command", "command": ...}`) is
/// app-owned.
fn is_app_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(APP_HOOK_MARKER))
}

/// Whether a `SessionStart` matcher group contains an app-owned inner hook.
fn group_has_app_hook(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|inner| inner.iter().any(is_app_hook))
}

/// Write-then-rename so a crash mid-write never leaves a truncated
/// settings.json behind.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), SettingsError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let file_name = path
        .file_name()
        .ok_or(SettingsError::UnexpectedShape("settings path"))?
        .to_string_lossy();
    let tmp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `YYYYMMDD-HHMMSS-mmm` in UTC, for backup file names. Hand-rolled
/// (Hinnant's civil-from-days) to avoid pulling in a date crate for one
/// format call.
fn utc_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let second_of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}-{millis:03}",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60,
    )
}

/// Days-since-epoch → (year, month, day), Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Anonymized copy of a real, heavily-customized settings.json (no env
    /// or hooks blocks) - see `tests/fixtures/settings/README.md`.
    const REALWORLD: &str = include_str!("../tests/fixtures/settings/realworld.json");
    /// Mixed env block: user var, app key at app value, app key at a
    /// different value, foreign OTel keys.
    const PREEXISTING_ENV: &str = include_str!("../tests/fixtures/settings/preexisting_env.json");
    /// User SessionStart + PostToolUse hook groups.
    const PREEXISTING_HOOKS: &str =
        include_str!("../tests/fixtures/settings/preexisting_hooks.json");
    /// Truncated JSON: the engine must abort and never write.
    const MALFORMED: &str = include_str!("../tests/fixtures/settings/malformed.json");

    struct Setup {
        _dir: TempDir,
        settings: PathBuf,
        backups: PathBuf,
    }

    /// Settings file (with `contents`, or missing when `None`) plus a
    /// backups dir path, both inside a temp dir.
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

    fn parse(raw: &str) -> Map<String, Value> {
        serde_json::from_str::<Value>(raw)
            .expect("fixture parses")
            .as_object()
            .expect("fixture is an object")
            .clone()
    }

    fn read_back(path: &Path) -> Map<String, Value> {
        read_settings(path).expect("read back")
    }

    fn env_of(map: &Map<String, Value>) -> &Map<String, Value> {
        map.get("env")
            .and_then(Value::as_object)
            .expect("env object")
    }

    fn session_start_of(map: &Map<String, Value>) -> &Vec<Value> {
        map.get("hooks")
            .and_then(|hooks| hooks.get("SessionStart"))
            .and_then(Value::as_array)
            .expect("SessionStart array")
    }

    fn backup_files(dir: &Path) -> Vec<PathBuf> {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .map(|entry| entry.expect("dir entry").path())
                .collect(),
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => panic!("read backups dir: {err}"),
        }
    }

    // ---- missing / empty files ----

    #[test]
    fn missing_file_merges_to_exactly_app_content() {
        let s = setup(None);
        let outcome = merge_file(&s.settings, &s.backups).expect("merge");
        assert!(outcome.changed);
        assert_eq!(outcome.backup_path, None, "nothing existed to back up");

        let merged = read_back(&s.settings);
        assert_eq!(merged.len(), 2, "only env + hooks: {merged:?}");
        assert_eq!(env_of(&merged).len(), APP_ENV.len());
        for (key, value) in APP_ENV {
            assert_eq!(env_of(&merged).get(key), Some(&Value::String(value.into())));
        }
        assert_eq!(session_start_of(&merged).len(), 1);
        assert!(is_installed(&merged));
    }

    #[test]
    fn empty_file_treated_as_no_settings() {
        let s = setup(Some("  \n"));
        let outcome = merge_file(&s.settings, &s.backups).expect("merge");
        assert!(outcome.changed);
        // The (empty) file existed, so it was backed up.
        let backup = outcome.backup_path.expect("backup written");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "  \n");
        assert!(is_installed(&read_back(&s.settings)));
    }

    // ---- real-world large settings ----

    #[test]
    fn realworld_merge_adds_exactly_five_env_keys_and_one_hook() {
        let s = setup(Some(REALWORLD));
        let original = parse(REALWORLD);
        merge_file(&s.settings, &s.backups).expect("merge");
        let merged = read_back(&s.settings);

        // Exactly env + hooks added at the top level.
        assert_eq!(merged.len(), original.len() + 2);
        assert_eq!(env_of(&merged).len(), APP_ENV.len());
        assert_eq!(session_start_of(&merged).len(), 1);

        // Zero data loss: every original key/value survives byte-for-value.
        for (key, value) in &original {
            assert_eq!(merged.get(key), Some(value), "lost or changed `{key}`");
        }
        // Key order preserved (preserve_order): original keys lead, in order.
        let merged_keys: Vec<&String> = merged.keys().collect();
        let original_keys: Vec<&String> = original.keys().collect();
        assert_eq!(&merged_keys[..original_keys.len()], &original_keys[..]);
    }

    #[test]
    fn realworld_unmerge_roundtrips_to_original() {
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("merge");
        let outcome = unmerge_file(&s.settings, &s.backups).expect("unmerge");
        assert!(outcome.changed);
        assert_eq!(read_back(&s.settings), parse(REALWORLD));
    }

    #[test]
    fn merge_is_idempotent() {
        let s = setup(Some(REALWORLD));
        let first = merge_file(&s.settings, &s.backups).expect("first merge");
        assert!(first.changed);
        let after_first = std::fs::read_to_string(&s.settings).unwrap();

        let second = merge_file(&s.settings, &s.backups).expect("second merge");
        assert!(!second.changed, "second merge must be a no-op");
        assert_eq!(second.backup_path, None, "no-op takes no backup");
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), after_first);
        assert_eq!(session_start_of(&read_back(&s.settings)).len(), 1);
        assert_eq!(
            backup_files(&s.backups).len(),
            1,
            "only the first merge backs up"
        );
    }

    // ---- pre-existing env block ----

    #[test]
    fn preexisting_env_is_preserved_and_conflicts_detected() {
        let original = parse(PREEXISTING_ENV);

        let conflicts = detect_conflicts(&original);
        let keys: Vec<&str> = conflicts.iter().map(|c| c.key.as_str()).collect();
        // App-owned key at a different value -> overwrite conflict.
        assert!(keys.contains(&"OTEL_LOGS_EXPORTER"));
        // Foreign telemetry keys -> informational conflicts.
        assert!(keys.contains(&"OTEL_EXPORTER_OTLP_ENDPOINT"));
        assert!(keys.contains(&"OTEL_METRICS_EXPORTER"));
        // App-owned key already at the app value -> not a conflict; plain
        // user vars -> not a conflict.
        assert!(!keys.contains(&"CLAUDE_CODE_ENABLE_TELEMETRY"));
        assert!(!keys.contains(&"MY_CUSTOM_VAR"));
        assert_eq!(conflicts.len(), 3);
        let overwrite = conflicts
            .iter()
            .find(|c| c.key == "OTEL_LOGS_EXPORTER")
            .unwrap();
        assert_eq!(overwrite.existing, Value::String("console".into()));
        assert_eq!(overwrite.proposed.as_deref(), Some("otlp"));

        let s = setup(Some(PREEXISTING_ENV));
        merge_file(&s.settings, &s.backups).expect("merge");
        let merged = read_back(&s.settings);
        let env = env_of(&merged);
        // User content intact, app keys at app values.
        assert_eq!(
            env.get("MY_CUSTOM_VAR"),
            Some(&Value::String("keep-me".into()))
        );
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some(&Value::String("http://collector.internal:4318".into())),
            "merge must not touch the foreign generic endpoint"
        );
        assert_eq!(
            env.get("OTEL_LOGS_EXPORTER"),
            Some(&Value::String("otlp".into()))
        );
        // 5 fixture keys + 5 app keys, 2 overlapping (telemetry flag +
        // logs exporter).
        assert_eq!(
            env.len(),
            5 + APP_ENV.len() - 2,
            "user keys + app keys, 2 shared"
        );
        assert_eq!(merged.get("permissions"), original.get("permissions"));
    }

    #[test]
    fn unmerge_removes_only_app_owned_env_keys() {
        let s = setup(Some(PREEXISTING_ENV));
        merge_file(&s.settings, &s.backups).expect("merge");
        unmerge_file(&s.settings, &s.backups).expect("unmerge");

        let unmerged = read_back(&s.settings);
        let env = env_of(&unmerged);
        for (key, _) in APP_ENV {
            assert!(!env.contains_key(key), "app key `{key}` must be removed");
        }
        assert_eq!(
            env.get("MY_CUSTOM_VAR"),
            Some(&Value::String("keep-me".into()))
        );
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some(&Value::String("http://collector.internal:4318".into()))
        );
        assert_eq!(
            env.get("OTEL_METRICS_EXPORTER"),
            Some(&Value::String("otlp".into()))
        );
        // Note: the fixture's CLAUDE_CODE_ENABLE_TELEMETRY="1" predates the
        // merge but equals the app value, so strict unmerge removes it -
        // value-match is the ownership test.
        assert!(
            !unmerged.contains_key("hooks"),
            "app-added hooks block removed"
        );
        assert_eq!(unmerged.get("model"), Some(&Value::String("sonnet".into())));
    }

    #[test]
    fn unmerge_leaves_user_edited_app_key_alone() {
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("merge");

        // User re-points the endpoint at their own collector post-install.
        let mut edited = read_back(&s.settings);
        edited["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"] =
            Value::String("http://localhost:4318/v1/logs".into());
        std::fs::write(&s.settings, serde_json::to_string_pretty(&edited).unwrap()).unwrap();

        unmerge_file(&s.settings, &s.backups).expect("unmerge");
        let unmerged = read_back(&s.settings);
        assert_eq!(
            env_of(&unmerged).get("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
            Some(&Value::String("http://localhost:4318/v1/logs".into())),
            "user-edited value means user ownership"
        );
        assert_eq!(env_of(&unmerged).len(), 1, "other app keys still removed");
    }

    // ---- pre-existing SessionStart hooks ----

    #[test]
    fn preexisting_hooks_are_kept_and_app_hook_appended() {
        let s = setup(Some(PREEXISTING_HOOKS));
        let original = parse(PREEXISTING_HOOKS);
        merge_file(&s.settings, &s.backups).expect("merge");

        let merged = read_back(&s.settings);
        let groups = session_start_of(&merged);
        assert_eq!(groups.len(), 2, "user group + app group");
        assert_eq!(
            &groups[0], &original["hooks"]["SessionStart"][0],
            "user group first, verbatim"
        );
        assert!(group_has_app_hook(&groups[1]));
        assert_eq!(
            merged["hooks"]["PostToolUse"], original["hooks"]["PostToolUse"],
            "unrelated hook events untouched"
        );
        assert!(is_installed(&merged));
    }

    #[test]
    fn unmerge_removes_only_app_hook_entries() {
        let s = setup(Some(PREEXISTING_HOOKS));
        merge_file(&s.settings, &s.backups).expect("merge");
        unmerge_file(&s.settings, &s.backups).expect("unmerge");
        assert_eq!(
            read_back(&s.settings),
            parse(PREEXISTING_HOOKS),
            "exact roundtrip"
        );
    }

    #[test]
    fn unmerge_keeps_user_hook_added_to_app_group_adjacent_config() {
        // User edits adjacent config after install: a new env var, their own
        // SessionStart group, and a new top-level key.
        let s = setup(Some(REALWORLD));
        merge_file(&s.settings, &s.backups).expect("merge");

        let mut edited = read_back(&s.settings);
        edited["env"]["USER_ADDED_LATER"] = Value::String("yes".into());
        edited["hooks"]["SessionStart"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks": [{"type": "command", "command": "echo hi"}]}));
        edited["hooks"]["Stop"] = json!([{"hooks": [{"type": "command", "command": "say done"}]}]);
        edited.insert("cleanupPeriodDays".into(), json!(30));
        std::fs::write(&s.settings, serde_json::to_string_pretty(&edited).unwrap()).unwrap();

        unmerge_file(&s.settings, &s.backups).expect("unmerge");
        let unmerged = read_back(&s.settings);
        assert_eq!(env_of(&unmerged).len(), 1);
        assert_eq!(
            env_of(&unmerged).get("USER_ADDED_LATER"),
            Some(&Value::String("yes".into()))
        );
        let groups = session_start_of(&unmerged);
        assert_eq!(groups.len(), 1, "only the user's group survives");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            Value::String("echo hi".into())
        );
        assert!(unmerged["hooks"].get("Stop").is_some());
        assert_eq!(unmerged.get("cleanupPeriodDays"), Some(&json!(30)));
        assert!(!is_installed(&unmerged));
    }

    // ---- malformed input: abort, never write ----

    #[test]
    fn malformed_json_aborts_without_writing() {
        let s = setup(Some(MALFORMED));
        let err = merge_file(&s.settings, &s.backups).expect_err("must abort");
        assert!(matches!(err, SettingsError::Malformed(_)), "got {err:?}");
        assert_eq!(
            std::fs::read_to_string(&s.settings).unwrap(),
            MALFORMED,
            "file bytes untouched"
        );
        assert!(backup_files(&s.backups).is_empty(), "no backup on abort");

        let err = unmerge_file(&s.settings, &s.backups).expect_err("unmerge must abort too");
        assert!(matches!(err, SettingsError::Malformed(_)));
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), MALFORMED);
    }

    #[test]
    fn non_object_root_aborts_without_writing() {
        let s = setup(Some("[1, 2, 3]\n"));
        let err = merge_file(&s.settings, &s.backups).expect_err("must abort");
        assert!(matches!(err, SettingsError::UnexpectedShape("root")));
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), "[1, 2, 3]\n");
        assert!(backup_files(&s.backups).is_empty());
    }

    #[test]
    fn wrong_shaped_env_aborts_without_writing() {
        let raw = r#"{"env": "not-an-object"}"#;
        let s = setup(Some(raw));
        let err = merge_file(&s.settings, &s.backups).expect_err("must abort");
        assert!(matches!(err, SettingsError::UnexpectedShape("env")));
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), raw);
        assert!(backup_files(&s.backups).is_empty());
    }

    // ---- error rendering for the UI (task 6.4) ----

    /// Every settings.json failure shown to the user names the file, and
    /// the common kinds carry a concrete remediation hint.
    #[test]
    fn describe_settings_error_names_file_and_remediation() {
        let path = Path::new("/tmp/settings.json");

        let malformed = serde_json::from_str::<Value>("{oops").expect_err("malformed");
        let message = describe_settings_error(&SettingsError::Malformed(malformed), path);
        assert!(message.contains("/tmp/settings.json"), "got: {message}");
        assert!(message.contains("Fix the JSON syntax"), "got: {message}");
        assert!(message.contains("has not been modified"), "got: {message}");

        let denied = std::io::Error::new(ErrorKind::PermissionDenied, "denied");
        let message = describe_settings_error(&SettingsError::Io(denied), path);
        assert!(message.contains("/tmp/settings.json"), "got: {message}");
        assert!(message.contains("permission"), "got: {message}");

        let full = std::io::Error::new(ErrorKind::StorageFull, "no space");
        let message = describe_settings_error(&SettingsError::Io(full), path);
        assert!(message.contains("disk is full"), "got: {message}");
        assert!(message.contains("intact"), "got: {message}");

        // Kinds without a specific hint still name the file.
        let shape = SettingsError::UnexpectedShape("env");
        let message = describe_settings_error(&shape, path);
        assert!(message.contains("/tmp/settings.json"), "got: {message}");
    }

    /// The real unreadable-file path end to end: a settings.json this user
    /// cannot read surfaces as an Io(PermissionDenied) the UI describes
    /// with the chmod hint, and nothing is ever written.
    #[test]
    #[cfg(unix)]
    fn unreadable_settings_file_degrades_with_permission_hint() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup(Some("{}"));
        std::fs::set_permissions(&s.settings, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        // Root can read anything; skip where the test runs privileged.
        if std::fs::read_to_string(&s.settings).is_ok() {
            return;
        }

        let err = read_settings(&s.settings).expect_err("must fail");
        assert!(
            matches!(&err, SettingsError::Io(io) if io.kind() == ErrorKind::PermissionDenied),
            "got {err:?}"
        );
        let message = describe_settings_error(&err, &s.settings);
        assert!(message.contains("chmod u+rw"), "got: {message}");
        assert!(
            message.contains(&s.settings.display().to_string()),
            "got: {message}"
        );

        let err = merge_file(&s.settings, &s.backups).expect_err("merge must abort");
        assert!(matches!(err, SettingsError::Io(_)), "got {err:?}");
        assert!(backup_files(&s.backups).is_empty(), "no backup on abort");

        std::fs::set_permissions(&s.settings, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms for cleanup");
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), "{}");
    }

    // ---- backups & restore ----

    #[test]
    fn backup_is_exact_copy_and_restore_works() {
        let s = setup(Some(REALWORLD));
        let outcome = merge_file(&s.settings, &s.backups).expect("merge");
        let backup = outcome.backup_path.expect("backup written");

        assert!(backup.starts_with(&s.backups));
        let name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("settings-2") && name.ends_with(".json"),
            "timestamped name, got {name}"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            REALWORLD,
            "byte-exact copy"
        );

        // Restore puts the original bytes back, even after further damage.
        std::fs::write(&s.settings, "garbage").unwrap();
        restore_from_backup(&backup, &s.settings).expect("restore");
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), REALWORLD);
    }

    #[test]
    fn backups_never_overwrite_each_other() {
        let s = setup(Some(REALWORLD));
        let first = merge_file(&s.settings, &s.backups)
            .expect("merge")
            .backup_path
            .unwrap();
        let second = unmerge_file(&s.settings, &s.backups)
            .expect("unmerge")
            .backup_path
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(backup_files(&s.backups).len(), 2);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), REALWORLD);
    }

    #[test]
    fn restore_from_missing_backup_errors() {
        let s = setup(Some(REALWORLD));
        let err =
            restore_from_backup(&s.backups.join("nope.json"), &s.settings).expect_err("must error");
        assert!(matches!(err, SettingsError::BackupMissing(_)));
        assert_eq!(std::fs::read_to_string(&s.settings).unwrap(), REALWORLD);
    }

    // ---- unmerge edge cases ----

    #[test]
    fn unmerge_of_missing_file_is_a_noop() {
        let s = setup(None);
        let outcome = unmerge_file(&s.settings, &s.backups).expect("unmerge");
        assert!(!outcome.changed);
        assert!(!s.settings.exists(), "no file conjured into existence");
    }

    #[test]
    fn unmerge_without_install_is_a_noop() {
        let s = setup(Some(REALWORLD));
        let outcome = unmerge_file(&s.settings, &s.backups).expect("unmerge");
        assert!(!outcome.changed);
        assert_eq!(
            std::fs::read_to_string(&s.settings).unwrap(),
            REALWORLD,
            "bytes untouched"
        );
        assert!(backup_files(&s.backups).is_empty());
    }

    #[test]
    fn is_installed_reflects_state() {
        assert!(!is_installed(&parse(REALWORLD)));
        assert!(
            !is_installed(&parse(PREEXISTING_ENV)),
            "partial env is not installed"
        );
        let merged = apply_merge(&parse(REALWORLD)).unwrap();
        assert!(is_installed(&merged));
        assert!(!is_installed(&apply_unmerge(&merged)));
    }
}
