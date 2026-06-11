// Typed wrappers around the faceted query layer Tauri commands
// (src-tauri/src/queries.rs, task 5.2). Tasks 5.3-5.6 build the analysis
// views on these; the payload shapes mirror the Rust serde contracts
// exactly (asserted by the queries.rs serde tests).

import { invoke } from "@tauri-apps/api/core";
import { UNKNOWN_PROJECT_OPTION, type FacetSelection } from "$lib/facets.svelte";

/** Date-range facet: a preset or an explicit [start, end) unix-ms window. */
export type RangeFacet =
  | "day"
  | "week"
  | "month"
  | "all"
  | { custom: { start_ms: number; end_ms: number } };

/** Project facet: everything, the unknown-project bucket, or one cwd. */
export type ProjectFacet = "all" | "unknown" | { cwd: string };

/** The shared facet parameters every aggregation command accepts. */
export interface Facets {
  range?: RangeFacet;
  project?: ProjectFacet;
  /** Exact model name; null/omitted = all models. */
  model?: string | null;
  query_source?: "all" | "main" | "subagent";
}

/** Local midnight (unix ms) of a `yyyy-mm-dd` date plus `dayOffset` days;
 * null when the string isn't a complete date. DST-correct: `Date` resolves
 * calendar components in the local zone, same contract as the backend's
 * preset boundaries. */
function localMidnightMs(isoDate: string, dayOffset = 0): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate);
  if (!match) return null;
  return new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]) + dayOffset).getTime();
}

/** Resolve the range selection: presets pass through; a custom selection
 * becomes an explicit `[start, end)` window (both picked dates inclusive,
 * so the end bound is the next local midnight). Incomplete custom dates
 * fall back to "all" until both are picked. */
function toRange(selection: FacetSelection): RangeFacet {
  if (selection.range !== "custom") return selection.range;
  const startMs = localMidnightMs(selection.customStart);
  const endMs = localMidnightMs(selection.customEnd, 1);
  if (startMs === null || endMs === null) return "all";
  return { custom: { start_ms: startMs, end_ms: endMs } };
}

/** Convert the UI facet state (task 5.1) into command parameters. */
export function toFacets(selection: FacetSelection): Facets {
  const project = selection.project.trim();
  const model = selection.model.trim();
  return {
    range: toRange(selection),
    project:
      project === "" ? "all" : project === UNKNOWN_PROJECT_OPTION ? "unknown" : { cwd: project },
    model: model === "" ? null : model,
    query_source: selection.querySource,
  };
}

/** The aggregate values every rollup shares. */
export interface Aggregates {
  /** API-equivalent cost; unpriced rows contribute nothing. */
  cost_usd: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  /** api_request rows (errors excluded). */
  requests: number;
  /** api_request rows with unknown model pricing: tokens counted, cost excluded. */
  unpriced_requests: number;
}

/** Headline totals for one facet selection. */
export interface UsageSummary extends Aggregates {
  /** Resolved window (unix ms); null = unbounded ("all"). */
  start_ms: number | null;
  end_ms: number | null;
  /** 5m/1h cache-creation split; null when no matching row carries it. */
  cache_creation_5m_tokens: number | null;
  cache_creation_1h_tokens: number | null;
  /** api_error rows in the window. */
  errors: number;
  /** Distinct session ids (resumes never double-count). */
  sessions: number;
}

/** One bucket (x group key) in the per-day series. */
export interface SeriesPoint extends Aggregates {
  /** Bucket opening instant (unix ms, inclusive); local midnight except
   * for a custom range's clamped first bucket. */
  bucket_start_ms: number;
  /** Group key (model or project cwd); null for ungrouped points and the
   * unknown-model/unknown-project bucket. */
  key: string | null;
  /** 5m/1h cache-creation split; null when no matching row carries it
   * (the split is transcript-exclusive). */
  cache_creation_5m_tokens: number | null;
  cache_creation_1h_tokens: number | null;
}

/** One session's rollup. */
export interface SessionRollup {
  session_id: string;
  /** Project directory; null = unknown project (no cwd mapping). */
  cwd: string | null;
  /** First/last request timestamps inside the facet window (unix ms). */
  first_ms: number;
  last_ms: number;
  cost_usd: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  requests: number;
  unpriced_requests: number;
  /** api_error rows in the window. */
  errors: number;
  /** Distinct models used, sorted. */
  models: string[];
}

/** One request in a session's drill-in timeline. */
export interface RequestDetail {
  timestamp_ms: number;
  model: string | null;
  /** Request origin tag (subagent, user, sdk, …); null = main. */
  query_source: string | null;
  /** "api_request" or "api_error". */
  event_type: string;
  /** Data source tag: "otel" (live) or "backfill" (transcript). */
  source: string;
  /** API-equivalent cost; null = unpriced (or an error row). */
  cost_usd: number | null;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  /** 5m/1h cache-creation split where backfill data provides it. */
  cache_creation_5m_tokens: number | null;
  cache_creation_1h_tokens: number | null;
  duration_ms: number | null;
  error: string | null;
}

/** One model's share of a session (the drill-in model mix). */
export interface ModelMix extends Aggregates {
  /** null = rows with no model recorded (typically error rows). */
  model: string | null;
}

/** A session's drill-in detail; the table's facets apply, so it always
 * reconciles with the rollup row that was clicked. */
export interface SessionDetail {
  session_id: string;
  /** Project directory; null = unknown project (no cwd mapping). */
  cwd: string | null;
  /** All matching rows (requests + errors); the timeline is capped at
   * 1000 rows, this count never is. */
  total_rows: number;
  /** Per-request timeline, timestamp ascending. */
  requests: RequestDetail[];
  /** Per-model aggregates over all matching rows, cost-descending. */
  models: ModelMix[];
}

/** One project's rollup. */
export interface ProjectRollup extends Aggregates {
  /** Project directory; null = the unknown-project bucket. */
  cwd: string | null;
  /** Distinct sessions that touched this project in the window. */
  sessions: number;
}

/** The option lists the facet bar offers. */
export interface FacetOptions {
  /** Distinct known project directories, sorted. */
  projects: string[];
  /** Whether an "unknown project" bucket exists. */
  unknown_project: boolean;
  /** Distinct model names observed, sorted. */
  models: string[];
}

export type SeriesGroupBy = "none" | "model" | "project";
export type SessionSort = "cost" | "tokens" | "duration" | "start";

/** Read-only: faceted headline totals. */
export function getUsageSummary(facets: Facets): Promise<UsageSummary> {
  return invoke<UsageSummary>("usage_summary", { facets });
}

/** Read-only: faceted per-local-day series, optionally grouped. */
export function getUsageSeries(
  facets: Facets,
  groupBy: SeriesGroupBy = "none"
): Promise<SeriesPoint[]> {
  return invoke<SeriesPoint[]>("usage_series", { facets, groupBy });
}

/** Read-only: faceted per-session rollups, sorted and paged in SQL. */
export function getSessionRollups(
  facets: Facets,
  options: {
    sort?: SessionSort;
    descending?: boolean;
    limit?: number;
    offset?: number;
  } = {}
): Promise<SessionRollup[]> {
  return invoke<SessionRollup[]>("session_rollups", { facets, ...options });
}

/** Read-only: one session's drill-in detail under the same facets. */
export function getSessionDetail(sessionId: string, facets: Facets): Promise<SessionDetail> {
  return invoke<SessionDetail>("session_detail", { sessionId, facets });
}

/** Read-only: faceted per-project rollups, cost-descending. */
export function getProjectRollups(facets: Facets): Promise<ProjectRollup[]> {
  return invoke<ProjectRollup[]>("project_rollups", { facets });
}

/** Read-only: the project/model option lists for the facet bar. */
export function getFacetOptions(): Promise<FacetOptions> {
  return invoke<FacetOptions>("facet_options");
}
