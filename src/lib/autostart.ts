// Typed wrappers around the autostart Tauri commands
// (src-tauri/src/autostart.rs).

import { invoke } from "@tauri-apps/api/core";

export interface AutostartStatus {
  /** Whether the LaunchAgent is registered right now (read live, never cached). */
  enabled: boolean;
  /**
   * Debug builds refuse to enable (the LaunchAgent would point at the dev
   * binary); the settings UI explains the read-only toggle with this.
   */
  dev_build: boolean;
}

/** Read-only: live login-item state. */
export function getAutostartStatus(): Promise<AutostartStatus> {
  return invoke<AutostartStatus>("autostart_status");
}

/**
 * Toggle the login item. Returns the re-read state so the UI reflects what
 * the plugin actually did, not the requested value.
 */
export function setAutostart(enabled: boolean): Promise<AutostartStatus> {
  return invoke<AutostartStatus>("autostart_set", { enabled });
}
