// Typed wrapper around the health Tauri command (src-tauri/src/health.rs).

import { invoke } from "@tauri-apps/api/core";
import type { BackfillInfo } from "$lib/backfill";
import type { Conflict } from "$lib/onboarding";

export interface ReceiverStatus {
  state: "starting" | "listening" | "port_in_use" | "failed";
  port?: number;
  message?: string;
}

export type ConfigState =
  | { state: "installed" }
  | { state: "missing" }
  | { state: "conflicting"; installed: boolean; conflicts: Conflict[] }
  | { state: "error"; message: string };

/** Since-launch ingest counters (task 1.4). */
export interface IngestStats {
  events_ingested: number;
  ingest_failures: number;
  events_skipped: number;
  /** Wall-clock ms of the last live ingest; 0 = never this launch. */
  last_event_ms: number;
  /** Detail of the most recent ingest failure; null when none ever. */
  last_failure: string | null;
}

export interface Cause {
  kind:
    | "capture_paused"
    | "port_conflict"
    | "receiver_failed"
    | "receiver_starting"
    | "sessions_predate_config"
    | "idle";
  detail: string;
}

/** The "configured but no events in N minutes" state. */
export interface NoEventsDiagnosis {
  threshold_minutes: number;
  /** Minutes since the last event; null when none was ever received. */
  minutes_since_last: number | null;
  /** Likely causes, most definitive first. */
  causes: Cause[];
}

/** Transcripts root used by backfill, and whether it exists yet. */
export interface TranscriptsInfo {
  path: string;
  exists: boolean;
}

export interface HealthStatus {
  receiver: ReceiverStatus;
  config: ConfigState;
  settings_path: string;
  /** While true, arriving events are acknowledged but discarded (task 4.4). */
  capture_paused: boolean;
  ingest: IngestStats;
  /** Unix ms of the most recent event received; null when none ever. */
  last_event_ms: number | null;
  /** All-time live-received event rows. */
  events_stored: number;
  /**
   * Set when stored-event totals could not be read from the database
   * (locked by another process, disk trouble); totals degrade to
   * since-launch counters.
   */
  db_error: string | null;
  /** Transcripts root used by backfill, and whether it exists. */
  transcripts: TranscriptsInfo;
  /** Transcript backfill: running flag + the last completed pass. */
  backfill: BackfillInfo;
  /** Present when the "configured but no events" detector fired. */
  no_events: NoEventsDiagnosis | null;
}

/** Read-only: the full diagnostics snapshot for the health view. */
export function getHealthStatus(): Promise<HealthStatus> {
  return invoke<HealthStatus>("health_status");
}
