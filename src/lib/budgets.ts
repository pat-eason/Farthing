// Typed wrapper around the budget-config Tauri commands and the shared
// budget event names (src-tauri/src/budgets.rs).

import { invoke } from "@tauri-apps/api/core";

/**
 * One budget threshold (daily or monthly). `notify` is persisted for the
 * deferred cost-notifications work and unused for now.
 */
export interface BudgetAmount {
  amount_usd: number;
  enabled: boolean;
  notify: boolean;
}

/**
 * Full budget configuration. `approach_pct` is persisted for the deferred
 * cost-notifications work and unused for now.
 */
export interface BudgetConfig {
  daily: BudgetAmount;
  monthly: BudgetAmount;
  show_in_tray: boolean;
  approach_pct: number;
}

/**
 * Emitted by the backend whenever budget config changes; payload is the
 * resulting (clamped) {@link BudgetConfig}.
 */
export const BUDGET_CONFIG_CHANGED = "budget:config-changed";

/**
 * Emitted by the coarse 60s tray-title tick; consumers refetch budget status
 * on this so the readout tracks live spend without polling.
 */
export const METRICS_TICK_EVENT = "metrics:tick";

/** Spend band, ordered green < yellow < amber < red (worst last). */
export type Band = "green" | "yellow" | "amber" | "red";

/** One budget's current state (daily or monthly). */
export interface BudgetLine {
  amount_usd: number;
  spent_priced_usd: number;
  unpriced_requests: number;
  /** Rounded percent of the budget spent. */
  percent: number;
  band: Band;
  /** spent_priced_usd >= amount_usd. */
  exceeded: boolean;
}

/**
 * Budget status for the tray/desktop readouts. A line is `null` when its
 * budget is unset/disabled; `worst_band` is the max band across set budgets
 * (`"green"` when none are set).
 */
export interface BudgetStatus {
  daily: BudgetLine | null;
  monthly: BudgetLine | null;
  show_in_tray: boolean;
  worst_band: Band;
}

/** Read-only: current budget config (settings view on open). */
export function getBudgetConfig(): Promise<BudgetConfig> {
  return invoke<BudgetConfig>("budget_config_get");
}

/** Read-only: current budget status (percent, band, worst-state) vs live spend. */
export function getBudgetStatus(): Promise<BudgetStatus> {
  return invoke<BudgetStatus>("budget_status");
}

/**
 * Persist budget config. Enabled amounts below $1.00 are clamped server-side;
 * returns the resulting (clamped) config.
 */
export function setBudgetConfig(config: BudgetConfig): Promise<BudgetConfig> {
  return invoke<BudgetConfig>("budget_config_set", { config });
}
