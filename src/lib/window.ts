// Typed wrapper around window-management Tauri commands
// (src-tauri/src/tray.rs, task 5.1).

import { invoke } from "@tauri-apps/api/core";

/**
 * Open (or re-focus) the desktop window from the popover. The backend hides
 * the popover, flips the macOS activation policy to Regular (Dock icon
 * appears), and shows + focuses the `main` window.
 */
export function openMainWindow(): Promise<void> {
  return invoke<void>("open_main_window");
}
