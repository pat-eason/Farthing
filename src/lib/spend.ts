// Typed wrappers around the cost-alert config Tauri commands and the
// config-changed event name (src-tauri/src/alerts.rs, cost-notifications plan).
//
// Mirrors the config/runtime DTOs serialized by `AlertState`. The Rust side
// uses `null` for an absent quiet window (the JSON `meta` boundary); everywhere
// else prefer `undefined`.

import { invoke } from "@tauri-apps/api/core";

/**
 * A quiet-hours window as wall-clock local `"HH:MM"` 24-hour strings, sized to
 * bind directly to native `<input type="time">` fields. A wrap-around window
 * (`start` later than `end`, e.g. "22:00"–"07:00") means overnight; the backend
 * resolves membership wrap-aware. `start === end` is treated as unset by the UI
 * and never persisted as a window.
 */
export interface QuietWindow {
  /** Inclusive start of quiet hours, local "HH:MM". */
  start: string;
  /** Exclusive end of quiet hours, local "HH:MM". */
  end: string;
}

/** The recurring-delta rule ("every $N of spend"); disabled by default. */
export interface DeltaConfig {
  enabled: boolean;
  /** Spend increment between milestones, in API-equivalent USD (default 50). */
  step_usd: number;
  /** Per-rule quiet hours, or `null` when always allowed (the JSON boundary). */
  quiet: QuietWindow | null;
}

/** The session/burst rate rule ("$N in a rolling window"); enabled by default. */
export interface BurstConfig {
  enabled: boolean;
  /** Spend in the window that arms the alert, in API-equivalent USD (default 10). */
  threshold_usd: number;
  /** Rolling-window width in minutes (default 10). */
  window_minutes: number;
  /** Minimum gap between burst fires in minutes (default 15). */
  cooldown_minutes: number;
  /** Per-rule quiet hours, or `null` when always allowed (the JSON boundary). */
  quiet: QuietWindow | null;
}

/** The full alert config (`alert_config_get` / `alert_config_set` payload). */
export interface AlertConfig {
  delta: DeltaConfig;
  burst: BurstConfig;
  /** "I pay per-token" flag: switches alert copy to real-money wording. */
  api_billing: boolean;
}

/** Delta dedup bookkeeping (the month a baseline tracks + the last step fired). */
export interface DeltaRuntime {
  /** Calendar month the baseline belongs to, e.g. "2026-06". */
  month_key: string;
  /** Highest milestone index already fired this month. */
  last_step: number;
}

/** Burst dedup bookkeeping (the cooldown deadline a fire arms). */
export interface BurstRuntime {
  /** Unix ms before which burst will not fire again; 0 means unarmed. */
  cooldown_until_ms: number;
}

/** The full alert runtime: edge-trigger/cooldown state + the permission signal. */
export interface AlertRuntime {
  delta: DeltaRuntime;
  burst: BurstRuntime;
  /** Set when a notification could not be delivered (permission revoked/denied). */
  permission_lost: boolean;
}

/**
 * Emitted by the backend after the alert config is saved; payload is the
 * resulting {@link AlertConfig}. Other windows refetch on this.
 */
export const ALERT_CONFIG_CHANGED_EVENT = "alert:config-changed";

/** Read-only: the current alert config (Spend UI reads this on mount). */
export function getAlertConfig(): Promise<AlertConfig> {
  return invoke<AlertConfig>("alert_config_get");
}

/**
 * Persist a new alert config. The backend re-evaluates and emits
 * {@link ALERT_CONFIG_CHANGED_EVENT}; returns the saved config so the UI
 * reflects what was stored.
 */
export function setAlertConfig(config: AlertConfig): Promise<AlertConfig> {
  return invoke<AlertConfig>("alert_config_set", { config });
}
