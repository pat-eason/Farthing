//! Autostart / login-item registration (task 2.3).
//!
//! Thin layer over `tauri-plugin-autostart` in LaunchAgent mode (per PRD):
//! `enable()` writes `~/Library/LaunchAgents/<app>.plist` pointing at the
//! installed bundle, `disable()` removes it, and `is_enabled()` reads the
//! plist's presence. State is never cached here; the settings UI always
//! reflects what the plugin reports, so out-of-band changes (user deletes
//! the plist by hand) show up on the next status query.
//!
//! Dev-build guard: debug builds refuse to *enable*. A LaunchAgent written
//! from a dev run would point at `target/debug/claude-usage-tracker`, break
//! on the next rebuild, and litter `~/Library/LaunchAgents`. Status queries
//! and disable (cleanup) remain available in dev so the settings UI and
//! tests can exercise real plugin state without registering anything.

use serde::Serialize;
use tauri::Runtime;
use tauri_plugin_autostart::ManagerExt;

/// Error returned when a debug build tries to enable autostart.
pub const DEV_BUILD_REFUSAL: &str =
    "autostart is disabled in dev builds: the LaunchAgent would point at the dev binary";

/// Current login-item state, read live from the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutostartStatus {
    /// Whether the LaunchAgent is registered right now.
    pub enabled: bool,
    /// `true` in debug builds, where enabling is refused. The settings UI
    /// uses this to explain why the toggle is read-only.
    pub dev_build: bool,
}

/// Best-effort result of the onboarding auto-enable. Never an error type:
/// autostart failure must not fail (or roll back) the settings.json merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OnboardingAutostart {
    pub enabled: bool,
    /// Why autostart is not enabled (dev build, plugin error), for the
    /// onboarding done screen.
    pub note: Option<String>,
}

/// Read the live plugin state. Read-only: never writes or removes a plist.
pub fn current_status<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<AutostartStatus, String> {
    let enabled = app
        .autolaunch()
        .is_enabled()
        .map_err(|err| format!("cannot read autostart state: {err}"))?;
    Ok(AutostartStatus {
        enabled,
        dev_build: cfg!(debug_assertions),
    })
}

/// Set the login-item state and return the resulting (re-read) status.
///
/// Enabling is refused in debug builds (see module docs). Disabling is
/// always allowed and idempotent: it only removes the plist when one is
/// registered, so toggling off a never-enabled machine is a no-op.
pub fn set_enabled<R: Runtime>(
    app: &tauri::AppHandle<R>,
    enabled: bool,
) -> Result<AutostartStatus, String> {
    let manager = app.autolaunch();
    if enabled {
        if cfg!(debug_assertions) {
            return Err(DEV_BUILD_REFUSAL.to_string());
        }
        manager
            .enable()
            .map_err(|err| format!("cannot enable autostart: {err}"))?;
    } else if manager
        .is_enabled()
        .map_err(|err| format!("cannot read autostart state: {err}"))?
    {
        manager
            .disable()
            .map_err(|err| format!("cannot disable autostart: {err}"))?;
    }
    current_status(app)
}

/// Onboarding hook: enable autostart after a successful settings.json merge
/// (the PRD's "registered as a login item so the receiver is always up").
/// Best-effort by design; the merge outcome is already final when this runs.
pub fn enable_after_onboarding<R: Runtime>(app: &tauri::AppHandle<R>) -> OnboardingAutostart {
    match set_enabled(app, true) {
        Ok(status) => OnboardingAutostart {
            enabled: status.enabled,
            note: None,
        },
        Err(note) => OnboardingAutostart {
            enabled: false,
            note: Some(note),
        },
    }
}

/// Frontend query: live login-item state for the settings UI. Read-only.
#[tauri::command]
pub fn autostart_status<R: Runtime>(app: tauri::AppHandle<R>) -> Result<AutostartStatus, String> {
    current_status(&app)
}

/// Frontend action: the settings toggle. Returns the re-read state so the
/// UI reflects reality rather than the requested value.
#[tauri::command]
pub fn autostart_set<R: Runtime>(
    app: tauri::AppHandle<R>,
    enabled: bool,
) -> Result<AutostartStatus, String> {
    set_enabled(&app, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::test::MockRuntime;
    use tauri_plugin_autostart::MacosLauncher;

    // Real plugin, mock runtime: is_enabled() reads actual LaunchAgent
    // state for the mock app's name (never registered on any machine), and
    // the dev guard blocks every code path that could write a plist.
    fn mock_app() -> tauri::App<MockRuntime> {
        tauri::test::mock_builder()
            .plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                None,
            ))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app with autostart plugin")
    }

    #[test]
    fn status_reads_disabled_from_plugin_state() {
        let app = mock_app();
        let status = current_status(app.handle()).expect("status");
        assert!(!status.enabled, "no LaunchAgent registered for mock app");
        assert!(status.dev_build, "tests compile with debug_assertions");
    }

    #[test]
    fn dev_build_refuses_enable_and_registers_nothing() {
        let app = mock_app();
        let err = set_enabled(app.handle(), true).expect_err("must refuse");
        assert_eq!(err, DEV_BUILD_REFUSAL);
        assert!(!current_status(app.handle()).expect("status").enabled);
    }

    #[test]
    fn disable_when_not_enabled_is_a_noop() {
        let app = mock_app();
        let status = set_enabled(app.handle(), false).expect("disable");
        assert!(!status.enabled);
    }

    #[test]
    fn onboarding_enable_never_errors_and_explains_dev_skip() {
        let app = mock_app();
        let outcome = enable_after_onboarding(app.handle());
        assert!(!outcome.enabled);
        assert_eq!(outcome.note.as_deref(), Some(DEV_BUILD_REFUSAL));
    }

    #[test]
    fn status_serializes_for_frontend() {
        let status = AutostartStatus {
            enabled: true,
            dev_build: false,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"enabled": true, "dev_build": false})
        );
    }
}
