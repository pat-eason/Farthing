//! Menu bar presence (task 4.1): tray icon, popover shell, activation
//! policy, and the tray menu.
//!
//! Window model:
//! - `popover`: a small undecorated always-on-top window anchored under the
//!   tray icon. Left-clicking the tray toggles it; losing focus (click-away)
//!   hides it. Both windows are defined in `tauri.conf.json` and live for
//!   the whole app lifetime; show/hide only, never destroyed.
//! - `main`: the desktop app window, hidden at startup. Opening it (tray
//!   menu) flips the macOS activation policy to `Regular` so a Dock icon
//!   appears; closing it hides the window and flips back to `Accessory`
//!   so the Dock icon disappears while the app stays resident.
//!
//! The pause menu item only flips [`TrayState::paused`] for now; task 4.4
//! wires the receiver to discard (200 + drop) while paused and adds the
//! paused badge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, Window, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};

pub const MAIN_WINDOW: &str = "main";
pub const POPOVER_WINDOW: &str = "popover";

const MENU_OPEN_APP: &str = "open-app";
const MENU_PAUSE: &str = "pause";
const MENU_QUIT: &str = "quit";

/// Click-away dismissal hides the popover from the focus-loss handler. When
/// the click that stole focus was on the tray icon itself, a `Click` tray
/// event arrives right after the hide; without suppression that click would
/// instantly re-show the window, making the tray icon impossible to use as
/// a "close" toggle. Tray clicks landing within this window of an auto-hide
/// are treated as "the popover was open; leave it closed".
const REOPEN_SUPPRESS_WINDOW: Duration = Duration::from_millis(300);

/// Shared tray state: capture-pause flag (stub until 4.4 points the
/// receiver at it) plus the popover auto-hide timestamp used for tray-click
/// toggle suppression.
#[derive(Clone, Default)]
pub struct TrayState {
    paused: Arc<AtomicBool>,
    popover_hidden_at: Arc<Mutex<Option<Instant>>>,
}

impl TrayState {
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    fn note_popover_hidden(&self) {
        *self.popover_hidden_at.lock().expect("tray state poisoned") = Some(Instant::now());
    }

    fn suppress_reopen(&self) -> bool {
        self.suppress_reopen_at(Instant::now())
    }

    fn suppress_reopen_at(&self, now: Instant) -> bool {
        self.popover_hidden_at
            .lock()
            .expect("tray state poisoned")
            .is_some_and(|hidden_at| {
                now.saturating_duration_since(hidden_at) < REOPEN_SUPPRESS_WINDOW
            })
    }
}

/// Builds the tray icon + menu and switches the app to menu-bar-only mode.
/// Called once from the app `setup` hook.
pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    let state = TrayState::default();
    app.manage(state);

    // Menu-bar-only at startup: no Dock icon until the desktop window is
    // opened (see handle_menu_event / handle_window_event).
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    let open_app = MenuItem::with_id(
        app,
        MENU_OPEN_APP,
        "Open Claude Usage Tracker",
        true,
        None::<&str>,
    )?;
    let pause =
        CheckMenuItem::with_id(app, MENU_PAUSE, "Pause capture", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_app,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .expect("bundled window icon missing")
        .clone();

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        // Template image: macOS renders it as a monochrome glyph that adapts
        // to the menu bar appearance (dark/light).
        .icon_as_template(true)
        .tooltip("Claude Usage Tracker")
        .menu(&menu)
        // Left click toggles the popover; the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| handle_menu_event(app, &event, &pause))
        .on_tray_icon_event(|tray, event| {
            // Feed the positioner so Position::TrayBottomCenter knows where
            // the icon is (multi-monitor / scale-factor aware).
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(tray.app_handle());
            }
        })
        .build(app.handle())?;

    Ok(())
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: &MenuEvent, pause: &CheckMenuItem<R>) {
    match event.id().as_ref() {
        MENU_OPEN_APP => show_main_window(app),
        MENU_PAUSE => {
            // The check item toggles natively; mirror its state into the
            // shared flag the receiver will consult (task 4.4).
            let checked = pause.is_checked().unwrap_or(false);
            app.state::<TrayState>().set_paused(checked);
        }
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

/// Opens (or re-focuses) the desktop window and brings back the Dock icon.
fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    // Policy first so the app can become a regular foreground app before
    // the window asks for focus.
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Left tray click: hide the popover if it is showing, otherwise anchor it
/// under the tray icon and show it.
fn toggle_popover<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(POPOVER_WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    if app.state::<TrayState>().suppress_reopen() {
        // The click-away handler hid the popover for this same click on the
        // tray icon: the user meant "close".
        return;
    }
    let _ = window.move_window(Position::TrayBottomCenter);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Registered via `Builder::on_window_event` for every window.
pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    match (window.label(), event) {
        // Click-away dismissal for the popover.
        (POPOVER_WINDOW, WindowEvent::Focused(false)) if window.is_visible().unwrap_or(false) => {
            let _ = window.hide();
            window
                .app_handle()
                .state::<TrayState>()
                .note_popover_hidden();
        }
        // Closing the desktop window hides it (the app keeps running in the
        // menu bar) and drops the Dock icon again.
        (MAIN_WINDOW, WindowEvent::CloseRequested { api, .. }) => {
            api.prevent_close();
            let _ = window.hide();
            #[cfg(target_os = "macos")]
            let _ = window
                .app_handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_defaults_to_false_and_toggles() {
        let state = TrayState::default();
        assert!(!state.paused());
        state.set_paused(true);
        assert!(state.paused());
        state.set_paused(false);
        assert!(!state.paused());
    }

    #[test]
    fn paused_is_shared_across_clones() {
        let state = TrayState::default();
        let clone = state.clone();
        clone.set_paused(true);
        assert!(state.paused());
    }

    #[test]
    fn reopen_not_suppressed_without_prior_hide() {
        let state = TrayState::default();
        assert!(!state.suppress_reopen());
    }

    #[test]
    fn reopen_suppressed_immediately_after_auto_hide() {
        let state = TrayState::default();
        state.note_popover_hidden();
        assert!(state.suppress_reopen());
    }

    #[test]
    fn reopen_allowed_after_suppress_window_elapses() {
        let state = TrayState::default();
        state.note_popover_hidden();
        let later = Instant::now() + REOPEN_SUPPRESS_WINDOW + Duration::from_millis(1);
        assert!(!state.suppress_reopen_at(later));
    }
}
