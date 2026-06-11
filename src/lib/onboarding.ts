// Typed wrappers around the onboarding Tauri commands
// (src-tauri/src/onboarding.rs).

import { invoke } from "@tauri-apps/api/core";

/** Pre-existing telemetry setting the merge would interact with. */
export interface Conflict {
  key: string;
  existing: unknown;
  /**
   * The app value that would overwrite the user's, or null for foreign
   * telemetry keys the merge leaves untouched but the user should know
   * about (e.g. an OTel endpoint pointing at another collector).
   */
  proposed: string | null;
}

export interface DiffLine {
  kind: "context" | "add" | "remove";
  text: string;
}

export interface OnboardingStatus {
  installed: boolean;
  changed: boolean;
  conflicts: Conflict[];
  settings_path: string;
  before: string;
  after: string;
  diff: DiffLine[];
}

export interface ApplyOutcome {
  changed: boolean;
  backup_path: string | null;
}

/** Read-only: current config state + merge preview. */
export function getOnboardingStatus(): Promise<OnboardingStatus> {
  return invoke<OnboardingStatus>("onboarding_status");
}

/**
 * Apply the settings.json merge (backup first, atomic write). Must pass
 * acknowledgeConflicts=true when the status reported conflicts; the backend
 * refuses otherwise.
 */
export function applyOnboarding(acknowledgeConflicts: boolean): Promise<ApplyOutcome> {
  return invoke<ApplyOutcome>("onboarding_apply", { acknowledgeConflicts });
}
