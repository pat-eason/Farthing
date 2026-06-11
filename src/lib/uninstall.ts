// Typed wrappers around the uninstall Tauri commands
// (src-tauri/src/uninstall.rs).

import { invoke } from "@tauri-apps/api/core";
import type { DiffLine } from "./onboarding";

/** Everything the confirmation dialog states (read-only). */
export interface UninstallStatus {
  /** The app's config is fully present in settings.json. */
  installed: boolean;
  /** Whether the unmerge would change settings.json at all. */
  settings_changed: boolean;
  settings_path: string;
  /** Whether the LaunchAgent is currently registered (read live). */
  autostart_enabled: boolean;
  database_path: string;
  database_exists: boolean;
  /** Total on-disk size including WAL/shm sidecars. */
  database_size_bytes: number;
  /** Where settings.json backups live; never deleted by uninstall. */
  backups_dir: string;
  /** Line diff of settings.json before → after the unmerge. */
  diff: DiffLine[];
}

/** Per-step results for the done screen. */
export interface UninstallOutcome {
  settings_changed: boolean;
  backup_path: string | null;
  /** LaunchAgent state after the disable attempt (false = removed). */
  autostart_enabled: boolean;
  autostart_note: string | null;
  database_deleted: boolean;
  database_note: string | null;
}

/** Read-only: what an uninstall would remove right now. */
export function getUninstallStatus(): Promise<UninstallStatus> {
  return invoke<UninstallStatus>("uninstall_status");
}

/**
 * Run the uninstall: settings.json unmerge (backup first; an error there
 * aborts everything), LaunchAgent removal, and database deletion only when
 * deleteDatabase is true.
 */
export function applyUninstall(deleteDatabase: boolean): Promise<UninstallOutcome> {
  return invoke<UninstallOutcome>("uninstall_apply", { deleteDatabase });
}
