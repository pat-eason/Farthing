// Typed wrapper around the health Tauri command (src-tauri/src/health.rs).

import { invoke } from "@tauri-apps/api/core";
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
}

/** Placeholder until the transcript backfill engine ships (Epic 3). */
export interface BackfillStatus {
  state: "not_available";
}

export interface Cause {
  kind:
    | "port_conflict"
    | "receiver_failed"
    | "receiver_starting"
    | "sessions_predate_config"
    | "paused";
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

export interface HealthStatus {
  receiver: ReceiverStatus;
  config: ConfigState;
  settings_path: string;
  ingest: IngestStats;
  /** Unix ms of the most recent event received; null when none ever. */
  last_event_ms: number | null;
  /** All-time live-received event rows. */
  events_stored: number;
  backfill: BackfillStatus;
  /** Present when the "configured but no events" detector fired. */
  no_events: NoEventsDiagnosis | null;
}

/** Read-only: the full diagnostics snapshot for the health view. */
export function getHealthStatus(): Promise<HealthStatus> {
  return invoke<HealthStatus>("health_status");
}
