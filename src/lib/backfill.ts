// Typed wrappers around the backfill Tauri commands
// (src-tauri/src/backfill.rs): status, the "Backfill now" manual trigger,
// and the capture-completeness diff report (task 3.5).

import { invoke } from "@tauri-apps/api/core";

/** Line accounting from the transcript parser (task 3.2). */
export interface ParseStats {
  lines_read: number;
  assistant_lines: number;
  skipped_lines: number;
  malformed_lines: number;
  invalid_assistant_lines: number;
}

/** Outcome of one backfill pass (task 3.4). */
export interface BackfillSummary {
  files_discovered: number;
  files_read: number;
  files_reset: number;
  requests_seen: number;
  requests_inserted: number;
  requests_deduped: number;
  splits_filled: number;
  unknown_model_rows: number;
  sessions_created: number;
  sessions_healed: number;
  io_errors: number;
  parse: ParseStats;
  started_ms: number;
  finished_ms: number;
}

/** Point-in-time backfill state: running flag + the last completed pass. */
export interface BackfillInfo {
  running: boolean;
  last: BackfillSummary | null;
}

/**
 * Capture-completeness report: stored live (OTel) rows vs transcript ground
 * truth over a window. PRD target: missing_pct < 1%.
 */
export interface DiffReport {
  window_hours: number;
  window_start_ms: number;
  generated_ms: number;
  files_scanned: number;
  /** Ground truth: distinct transcript requestIds in the window. */
  transcript_requests: number;
  /** In transcripts and captured by the live OTel pipeline. */
  matched: number;
  /** In transcripts but missed live; stored only thanks to backfill. */
  backfill_only: number;
  /** Captured live but absent from transcripts (e.g. file cleaned up). */
  otel_only: number;
  /** backfill_only / transcript_requests as a percentage; null when the
   * window holds no transcript ground truth. */
  missing_pct: number | null;
  io_errors: number;
  parse: ParseStats;
}

/** Read-only: the current backfill state. */
export function getBackfillStatus(): Promise<BackfillInfo> {
  return invoke<BackfillInfo>("backfill_status");
}

/**
 * "Backfill now": run one incremental pass. Rejects when a pass is already
 * running (startup or a previous trigger).
 */
export function runBackfill(): Promise<BackfillSummary> {
  return invoke<BackfillSummary>("backfill_run");
}

/** Generate the capture-completeness report over the trailing window. */
export function getDiffReport(windowHours: number): Promise<DiffReport> {
  return invoke<DiffReport>("backfill_diff_report", { windowHours });
}
