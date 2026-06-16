# Linux support

Why the Linux port is bounded the way it is, which platform-conditional gates
exist in the codebase, and what the accepted limitations are. See the
implementation plan at
`docs/plans/2026-06-12-004-feat-subscription-mode-desktop-views-plan.md`.

## Scope

**Goals (v1 .deb / AppImage):**

- G1: Farthing compiles and runs on Ubuntu 22.04+ (x86-64).
- G2: The data pipeline (OTel receiver, transcript backfill, SQLite) is fully
  cross-platform — zero macOS-specific code in those modules.
- G3: The tray icon and popover work on GNOME/X11 and KDE/X11 (best-effort
  Wayland).
- G4: Autostart (XDG `.desktop` login item) works via `tauri-plugin-autostart`.
- G5: The tray title (cost + budget) is rendered as a plain text string.

**Non-goals (NG):**

- NG1: Windows support.
- NG2: Flatpak packaging.
- NG3: AUR packaging.
- NG4: CLI / headless mode.
- NG5: Pixel-perfect Wayland popover positioning (Mutter lacks `wlr-layer-shell`).
- NG6: Runtime GNOME AppIndicator extension detection or prompts.
- NG7: Ubuntu 20.04 support (requires WebKit2GTK 4.0; app requires 4.1).
- NG8: `wlr-layer-shell` popover anchoring.

## Platform-conditional inventory

All gates are **compile-time `cfg` attributes**. There is no runtime OS
branching in the codebase: `grep -rn "std::env::consts::OS" src-tauri/src/`
returns zero matches (verified 2026-06-15 against the feat-linux-support branch).

| File | Line | Gate | Behavior on Linux |
|---|---|---|---|
| `lib.rs` | 24–25 | `#[cfg(target_os = "macos")] pub mod tray_render;` | module not compiled |
| `lib.rs` | 42–45 | `MacosLauncher::LaunchAgent` passed to autostart plugin | compiles on all platforms; plugin ignores the argument on Linux and uses XDG autostart instead (see AD-1) |
| `tray.rs` | 96–97 | `set_activation_policy(Accessory)` at tray setup | gated out; no Dock concept on Linux |
| `tray.rs` | 212–213 | `set_activation_policy(Regular)` on desktop window show | gated out |
| `tray.rs` | 254–257 | `set_activation_policy(Accessory)` on desktop window close | gated out |
| `tray_title.rs` | 171–213 | `#[cfg(target_os = "macos")]` — stacked PNG budget readout via `tray_render` | gated out; macOS-only image rendering path |
| `tray_title.rs` | 215–226 | `#[cfg(not(target_os = "macos"))]` — `set_title` text branch | **the Linux path** — plain cost + budget string via `set_title` |
| `tray_title.rs` | 231–269 | `#[cfg(target_os = "macos")]` — `CUSTOM_IMAGE_ACTIVE` flag + `budget_render_model` | gated out |
| `Cargo.toml` | 69–98 | `[target.'cfg(target_os = "macos")'.dependencies]` — `objc2`, `objc2-foundation`, `objc2-app-kit` | not compiled or linked on Linux |

### Modules with zero platform gates (fully cross-platform)

`receiver.rs`, `ingest.rs`, `transcript.rs`, `backfill.rs`, `db.rs`,
`metrics.rs`, `queries.rs`, `settings_merge.rs`, `pricing.rs`, `session.rs`,
`budgets.rs`, `alerts.rs`, `capture.rs`, `export.rs`, `health.rs`, `notify.rs`,
`onboarding.rs`, `uninstall.rs`

### `tray_render.rs` — macOS-only module

Entire module is excluded from non-macOS builds by the `lib.rs` module gate.
It draws a stacked PNG status-button image via `objc2` / AppKit for the budget
tray readout; there is no Linux equivalent (text suffices: see G5).

### `autostart.rs` — cross-platform by plugin design

`MacosLauncher::LaunchAgent` is exported unconditionally by
`tauri-plugin-autostart` and compiles on all platforms. On macOS it writes
`~/Library/LaunchAgents/<id>.plist`; on Linux the plugin ignores it and writes
`~/.config/autostart/<id>.desktop`. No code change was required in `autostart.rs`.

## Accepted Linux limitations

**GNOME tray (AppIndicator):** GNOME requires the
`gnome-shell-extension-appindicator` extension (or equivalent) for a system tray
to appear at all. Ubuntu 22.04+ ships it pre-enabled; vanilla GNOME (Fedora,
Arch) and older Ubuntu do not. Farthing makes no attempt to detect or prompt for
this at runtime (NG6). The installer README covers it.

**Wayland popover positioning:** `tauri-plugin-positioner`'s
`Position::TrayBottomCenter` falls back to an approximate position under Wayland
(Mutter/GNOME lacks `wlr-layer-shell`). The popover window appears but may not
anchor precisely to the tray icon. This is accepted best-effort behavior (AD-3).

**WebKit2GTK floor:** Tauri v2 on Linux requires WebKit2GTK 4.1, which ships
with Ubuntu 22.04+. Ubuntu 20.04 (WebKit2GTK 4.0) is unsupported (NG7).

**AppImage / FUSE:** The AppImage distribution format requires FUSE 2 on the
host. Ubuntu 22.04+ ships it; some minimal installs do not. Standard mitigation
is documented in the AppImage project (`--appimage-extract-and-run` flag).

**Tray title emoji rendering:** The Linux path in `tray_title.rs` emits emoji
characters (stoplight dots: 🟢 🟡 🟠 🔴) as plain Unicode in the text title.
Rendering fidelity across desktop environments and tray implementations varies
and is a known cosmetic limitation (AD-5).

## Why no code change was needed in the cross-platform core

The data pipeline modules listed above have always been free of macOS-specific
imports; the Tauri and Tokio APIs they use compile on Linux without any
conditional compilation.

The only tray-visible functional difference is the title rendering path: the
macOS stacked-PNG budget readout (`tray_render.rs`) was gated from day one, and
the `#[cfg(not(target_os = "macos"))]` `set_title` branch in `tray_title.rs`
was the already-present fallback.

Evidence: the `rust-linux` CI job added in chunk-1 builds the project and runs
`cargo test` on Ubuntu 22.04 without modification to any `.rs` file, `Cargo.toml`,
or `tauri.conf.json`. All tests pass on that job, confirming that the existing
gates are complete and sufficient.
