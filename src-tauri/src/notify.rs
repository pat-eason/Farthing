//! Native desktop notification delivery (Unit 1 of the cost-notifications plan).
//!
//! A thin, permission-gated wrapper over `tauri-plugin-notification`. Every
//! send is gated on the OS permission state: if permission is not `Granted`,
//! [`show`] returns [`ShowOutcome::PermissionDenied`] and skips delivery
//! entirely. It never errors and never mutates alert state — the orchestrator
//! (a later unit) reads the outcome and records the permission-lost signal
//! itself. Keeping that decision out of here leaves this module dependency-free.
//!
//! Notifications are **display-only**: title + body, no click or action
//! handlers. Tauri 2's Actions API is mobile-only (tauri#3698), so the tray's
//! "Open Farthing" item is the desktop re-entry path.
//!
//! ## Testability seam
//!
//! The desktop plugin's `permission_state()` always returns `Granted` and its
//! `show()` hits the real OS notification center — neither is exercisable under
//! `MockRuntime` (which has no notification backend), and `tauri dev` delivery
//! on macOS is unreliable. So the permission gate lives behind a [`Notifier`]
//! trait: the production path delegates to `NotificationExt`, and tests inject
//! a fake to assert the gating contract (deliver iff `Granted`) without touching
//! the OS. Real OS delivery is verified only by manual testing of a bundled,
//! signed `.app` (see the plan's verification notes).

use serde::Serialize;
use tauri::plugin::PermissionState;
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

/// The two `rule_type` values [`notification_send_test`] accepts. The Spend UI
/// passes one of these to preview each alert's copy; unknown values are
/// rejected so a typo surfaces instead of silently sending nothing.
pub const RULE_TYPE_BURST: &str = "burst";
pub const RULE_TYPE_DELTA: &str = "delta";

/// Outcome of a [`show`] attempt. Reported back to the caller rather than
/// raised as an error: a denied permission is an expected, recoverable state
/// (the user revoked it, or never granted it), not a failure of the send path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ShowOutcome {
    /// Permission was `Granted`; the notification was handed to the OS.
    Delivered,
    /// Permission was not `Granted`; nothing was delivered. The caller should
    /// surface this as a "notifications are off" signal.
    PermissionDenied,
}

/// The permission-check + delivery seam (see module docs). Implemented for real
/// by [`PluginNotifier`] over `NotificationExt`; faked in tests so the gating
/// contract is verifiable without an OS notification backend.
trait Notifier {
    /// Current OS permission state for notifications.
    fn permission_state(&self) -> PermissionState;
    /// Hand a title/body to the OS notification center. Only ever called once
    /// the gate has confirmed `Granted`.
    fn deliver(&self, title: &str, body: &str);
}

/// Production [`Notifier`]: delegates to the notification plugin via
/// `NotificationExt`. A plugin error (permission read or builder send) is
/// treated as a non-delivery rather than propagated — the gate already decides
/// outcome, and a send failure is best-effort by design.
struct PluginNotifier<'a, R: Runtime> {
    app: &'a AppHandle<R>,
}

impl<R: Runtime> Notifier for PluginNotifier<'_, R> {
    fn permission_state(&self) -> PermissionState {
        // A failed read is the safe-closed default (Prompt): treat as not
        // granted rather than assuming permission.
        self.app
            .notification()
            .permission_state()
            .unwrap_or(PermissionState::Prompt)
    }

    fn deliver(&self, title: &str, body: &str) {
        let _ = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }
}

/// Gate a notification on permission state and (if `Granted`) deliver it.
///
/// Returns [`ShowOutcome::PermissionDenied`] without attempting delivery when
/// permission is anything but `Granted`. Never errors, never mutates alert
/// state — the caller records the permission-lost signal from the outcome.
pub fn show<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) -> ShowOutcome {
    show_with(&PluginNotifier { app }, title, body)
}

/// The pure gating logic, parameterized over the [`Notifier`] seam so tests can
/// inject a fake. Delivers iff permission is `Granted`.
fn show_with(notifier: &impl Notifier, title: &str, body: &str) -> ShowOutcome {
    if notifier.permission_state() == PermissionState::Granted {
        notifier.deliver(title, body);
        ShowOutcome::Delivered
    } else {
        ShowOutcome::PermissionDenied
    }
}

/// Representative placeholder copy for a test notification, per rule type. Never
/// reads live DB data — the sample values are fixed so "Send a test" previews
/// the shape of each alert without depending on actual spend.
fn test_copy(rule_type: &str) -> Result<(&'static str, &'static str), String> {
    match rule_type {
        RULE_TYPE_BURST => Ok((
            "Usage spike",
            "$12.40 in the last 10 minutes (sample)",
        )),
        RULE_TYPE_DELTA => Ok(("Usage milestone", "$50 of usage so far (sample)")),
        other => Err(format!("cannot send test notification: unknown rule type '{other}'")),
    }
}

/// Frontend query: current notification permission state as a string
/// (`"granted"`, `"denied"`, `"prompt"`, ...), matching the plugin's own
/// serialization so the Spend UI can branch on it.
#[tauri::command]
pub fn notification_permission_state<R: Runtime>(app: AppHandle<R>) -> String {
    app.notification()
        .permission_state()
        .unwrap_or(PermissionState::Prompt)
        .to_string()
}

/// Frontend action: request notification permission, returning the resulting
/// state as a string. macOS can only prompt once; a prior `Denied` returns
/// `"denied"` and the UI must deep-link to System Settings.
#[tauri::command]
pub fn notification_request_permission<R: Runtime>(app: AppHandle<R>) -> String {
    app.notification()
        .request_permission()
        .unwrap_or(PermissionState::Denied)
        .to_string()
}

/// Frontend action: deliver a test notification with placeholder copy for the
/// given rule type (`"burst"` or `"delta"`). Uses fixed sample values, never
/// live DB data, so the user sees the shape of an alert on demand.
#[tauri::command]
pub fn notification_send_test<R: Runtime>(app: AppHandle<R>, rule_type: String) -> Result<(), String> {
    let (title, body) = test_copy(&rule_type)?;
    show(&app, title, body);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Fake [`Notifier`] with an injectable permission state that records every
    /// delivery so tests can assert both the outcome and whether a send was
    /// even attempted.
    struct FakeNotifier {
        state: PermissionState,
        delivered: RefCell<Vec<(String, String)>>,
    }

    impl FakeNotifier {
        fn new(state: PermissionState) -> Self {
            Self {
                state,
                delivered: RefCell::new(Vec::new()),
            }
        }
    }

    impl Notifier for FakeNotifier {
        fn permission_state(&self) -> PermissionState {
            self.state
        }

        fn deliver(&self, title: &str, body: &str) {
            self.delivered
                .borrow_mut()
                .push((title.to_string(), body.to_string()));
        }
    }

    #[test]
    fn granted_delivers_via_the_seam() {
        let notifier = FakeNotifier::new(PermissionState::Granted);
        let outcome = show_with(&notifier, "Title", "Body");
        assert_eq!(outcome, ShowOutcome::Delivered);
        assert_eq!(
            *notifier.delivered.borrow(),
            vec![("Title".to_string(), "Body".to_string())]
        );
    }

    #[test]
    fn denied_skips_delivery() {
        let notifier = FakeNotifier::new(PermissionState::Denied);
        let outcome = show_with(&notifier, "Title", "Body");
        assert_eq!(outcome, ShowOutcome::PermissionDenied);
        assert!(
            notifier.delivered.borrow().is_empty(),
            "denied permission must not attempt delivery"
        );
    }

    /// `Prompt`/`Unknown` (anything not `Granted`) is gated the same as denied:
    /// never deliver without explicit grant.
    #[test]
    fn prompt_is_gated_like_denied() {
        let notifier = FakeNotifier::new(PermissionState::Prompt);
        let outcome = show_with(&notifier, "Title", "Body");
        assert_eq!(outcome, ShowOutcome::PermissionDenied);
        assert!(notifier.delivered.borrow().is_empty());
    }

    #[test]
    fn test_copy_is_sample_data_per_rule_type() {
        let (burst_title, burst_body) = test_copy(RULE_TYPE_BURST).expect("burst copy");
        assert_eq!(burst_title, "Usage spike");
        assert!(
            burst_body.contains("sample"),
            "test copy must be flagged as sample, not live data"
        );

        let (delta_title, delta_body) = test_copy(RULE_TYPE_DELTA).expect("delta copy");
        assert_eq!(delta_title, "Usage milestone");
        assert!(delta_body.contains("sample"));

        // The two rule types produce distinct copy.
        assert_ne!(burst_body, delta_body);
    }

    #[test]
    fn test_copy_rejects_unknown_rule_type() {
        let err = test_copy("forecast").expect_err("unknown rule type must error");
        assert!(err.contains("forecast"), "error names the bad rule type");
    }

    /// The command strings must match the plugin's own `PermissionState`
    /// serialization so the Spend UI can branch on a single vocabulary.
    #[test]
    fn permission_state_strings_match_plugin_vocabulary() {
        assert_eq!(PermissionState::Granted.to_string(), "granted");
        assert_eq!(PermissionState::Denied.to_string(), "denied");
        assert_eq!(PermissionState::Prompt.to_string(), "prompt");
    }

    /// `ShowOutcome` is serialized into command results / logs; lock its wire
    /// shape (unit-variant names) so a rename is a visible break.
    #[test]
    fn show_outcome_serializes_to_variant_names() {
        assert_eq!(
            serde_json::to_value(ShowOutcome::Delivered).unwrap(),
            serde_json::json!("Delivered")
        );
        assert_eq!(
            serde_json::to_value(ShowOutcome::PermissionDenied).unwrap(),
            serde_json::json!("PermissionDenied")
        );
    }
}
