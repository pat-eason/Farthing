// Typed wrapper around the subscription usage Tauri commands and events
// (src-tauri/src/usage_limits.rs).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Types (mirror the Rust types exactly)
// ---------------------------------------------------------------------------

/** Whether the Anthropic usage-limits API responded or not. */
export type UsageStatus = "ok" | "unauthenticated" | "unavailable";

/**
 * One usage window — the four windows are: 5-hour session, 7-day overall,
 * 7-day Sonnet, and 7-day Opus. `percent` and `resets_at_ms` are null when
 * the API didn't return them (e.g. the window has never been used).
 */
export interface WindowSnapshot {
  /** Human-readable label: "5h session", "7d overall", "7d Sonnet", "7d Opus". */
  label: string;
  /** 0–100 inclusive; null when unknown. */
  percent: number | null;
  /** Unix epoch milliseconds of next reset; null when unknown. */
  resets_at_ms: number | null;
}

/**
 * Extra/add-on usage credits on the subscription plan. Only present when
 * the user has this feature enabled on their account.
 */
export interface ExtraUsageSnapshot {
  is_enabled: boolean;
  monthly_limit: number | null;
  used_credits: number | null;
  /** 0.0–1.0 utilization ratio; null when monthly_limit is 0 or unknown. */
  utilization: number | null;
}

/**
 * Full snapshot returned by `usage_limits_status`. Includes all four usage
 * windows, optional extra-usage data, the fetch timestamp, and status.
 */
export interface UsageSnapshot {
  five_hour: WindowSnapshot;
  seven_day: WindowSnapshot;
  seven_day_sonnet: WindowSnapshot;
  seven_day_opus: WindowSnapshot;
  /** Null when the user's plan has no extra-usage add-on. */
  extra_usage: ExtraUsageSnapshot | null;
  /** Unix epoch milliseconds when this snapshot was fetched. */
  fetched_at_ms: number;
  status: UsageStatus;
}

/** Whether to show subscription plan limits or API token billing data. */
export type DisplayMode = "api" | "subscription";

/** Persisted config for the usage-limits feature. */
export interface UsageLimitsConfig {
  /** When false, the background poller is stopped and the view shows a banner. */
  enabled: boolean;
  display_mode: DisplayMode;
}

// ---------------------------------------------------------------------------
// Event names (emitted by src-tauri/src/usage_limits.rs)
// ---------------------------------------------------------------------------

/** Emitted after a successful or failed poll; payload is UsageSnapshot | null. */
export const USAGE_UPDATED_EVENT = "usage:updated";

/** Emitted when the user switches display mode; payload is DisplayMode. */
export const DISPLAY_MODE_CHANGED_EVENT = "display:mode-changed";

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

/**
 * Returns the most-recently-fetched usage snapshot, or null when no fetch has
 * completed yet (first launch before the 5-minute poll fires).
 */
export function getUsageStatus(): Promise<UsageSnapshot | null> {
  return invoke<UsageSnapshot | null>("usage_limits_status");
}

/** Read the persisted usage-limits config (enabled flag + display mode). */
export function getUsageLimitsConfig(): Promise<UsageLimitsConfig> {
  return invoke<UsageLimitsConfig>("usage_limits_config_get");
}

/** Persist the usage-limits config. Triggers a fresh poll when enabled transitions true. */
export function setUsageLimitsConfig(config: UsageLimitsConfig): Promise<void> {
  return invoke<void>("usage_limits_config_set", { config });
}

/** Read the current display mode without loading the full config. */
export function getDisplayMode(): Promise<DisplayMode> {
  return invoke<DisplayMode>("display_mode_get");
}

/** Persist the display mode only; emits DISPLAY_MODE_CHANGED_EVENT. */
export function setDisplayMode(mode: DisplayMode): Promise<void> {
  return invoke<void>("display_mode_set", { mode });
}

// ---------------------------------------------------------------------------
// Event listener helpers
// ---------------------------------------------------------------------------

/**
 * Subscribe to usage snapshot updates pushed from the backend poller.
 * Returns a cleanup function that removes the listener.
 *
 * Usage:
 *   const stop = await onUsageUpdated((snap) => { snapshot = snap; });
 *   // later: stop();
 */
export async function onUsageUpdated(
  cb: (snapshot: UsageSnapshot | null) => void
): Promise<() => void> {
  const unlisten = await listen<UsageSnapshot | null>(USAGE_UPDATED_EVENT, (event) => {
    cb(event.payload);
  });
  return unlisten;
}

/**
 * Subscribe to display-mode changes pushed from the backend.
 * Returns a cleanup function that removes the listener.
 */
export async function onDisplayModeChanged(cb: (mode: DisplayMode) => void): Promise<() => void> {
  const unlisten = await listen<DisplayMode>(DISPLAY_MODE_CHANGED_EVENT, (event) => {
    cb(event.payload);
  });
  return unlisten;
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/**
 * Format the time remaining until a usage window resets.
 *
 * Returns:
 *   - "resets in 2h 10m" when more than an hour remains
 *   - "resets in 45m" when less than an hour remains
 *   - "resets in <1m" when under one minute remains
 *   - "" when `resets_at_ms` is null or the reset time is in the past
 */
export function formatResetIn(resets_at_ms: number | null): string {
  if (resets_at_ms === null) return "";
  const msRemaining = resets_at_ms - Date.now();
  if (msRemaining <= 0) return "";
  const totalMinutes = Math.floor(msRemaining / 60_000);
  if (totalMinutes < 1) return "resets in <1m";
  if (totalMinutes < 60) return `resets in ${totalMinutes}m`;
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return minutes > 0 ? `resets in ${hours}h ${minutes}m` : `resets in ${hours}h`;
}
