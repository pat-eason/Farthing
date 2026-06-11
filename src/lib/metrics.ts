// Typed wrapper around the today-metrics Tauri command
// (src-tauri/src/metrics.rs).

import { invoke } from "@tauri-apps/api/core";

/** One project rollup in the top-projects list. */
export interface ProjectCost {
  /** Session working directory; null = sessions with no known cwd. */
  cwd: string | null;
  /** API-equivalent cost of this project's requests today. */
  cost_usd: number;
  /** api_request rows behind that cost. */
  requests: number;
}

/** Today's aggregates for the popover; "today" is the local calendar day. */
export interface TodayMetrics {
  /** Local midnight opening the window (unix ms, inclusive). */
  day_start_ms: number;
  /** Next local midnight closing the window (unix ms, exclusive). */
  day_end_ms: number;
  /** Total API-equivalent cost; unpriced rows contribute nothing. */
  cost_usd: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  /** api_request rows today (errors excluded). */
  requests: number;
  /** Rows with unknown model pricing: tokens counted, cost excluded. */
  unpriced_requests: number;
  /** Distinct session_ids active today; resumes don't double-count. */
  sessions: number;
  /** Top projects by cost, descending, at most 3. */
  top_projects: ProjectCost[];
}

/** Read-only: today's metrics for the popover. */
export function getTodayMetrics(): Promise<TodayMetrics> {
  return invoke<TodayMetrics>("today_metrics");
}

/** One day's cost bucket in the sparkline series. */
export interface DailyCost {
  /** Local midnight opening this day (unix ms, inclusive). */
  day_start_ms: number;
  /** API-equivalent cost; unpriced rows contribute nothing. */
  cost_usd: number;
  /** api_request rows that day (errors excluded). */
  requests: number;
}

/**
 * Read-only: per-day cost buckets for the trailing `days` local calendar
 * days, oldest first, today last. Gap days come back as explicit zero
 * buckets, so the result always has exactly `days` entries.
 */
export function getDailyCosts(days: number): Promise<DailyCost[]> {
  return invoke<DailyCost[]>("daily_costs", { days });
}
