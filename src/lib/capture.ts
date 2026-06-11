// Typed wrapper around the capture pause/resume Tauri commands and the
// live-update event names (src-tauri/src/capture.rs + ingest.rs, task 4.4).

import { invoke } from "@tauri-apps/api/core";

/** Pause state (`capture_status` command + paused-changed event payload). */
export interface CaptureStatus {
  paused: boolean;
}

/**
 * Emitted by the backend whenever the pause state changes (tray menu,
 * resume button); payload is a {@link CaptureStatus}.
 */
export const PAUSED_CHANGED_EVENT = "capture:paused-changed";

/**
 * Emitted after a `/v1/logs` export stores at least one row; payload is the
 * stored-row count. The popover refetches its metrics on this instead of
 * polling.
 */
export const INGESTED_EVENT = "ingest:stored";

/** Read-only: current capture pause state. */
export function getCaptureStatus(): Promise<CaptureStatus> {
  return invoke<CaptureStatus>("capture_status");
}

/**
 * Pause or resume capture. Persists across restarts and syncs the tray
 * menu check + paused badge; returns the resulting state.
 */
export function setCapturePaused(paused: boolean): Promise<CaptureStatus> {
  return invoke<CaptureStatus>("capture_set_paused", { paused });
}
