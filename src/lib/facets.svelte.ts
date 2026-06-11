// Global facet selections (task 5.1): one shared, mutable state object that
// every analysis view reads, so navigating between views never resets the
// active filters. Module-level $state lives for the webview's lifetime; the
// main window is hidden on close, never destroyed, so selections also
// survive close/reopen of the desktop window within an app run.
//
// Task 5.2's `toFacets` (queries.ts) turns these selections into the SQL
// facet params; tasks 5.3-5.6 feed them to their aggregation commands.

/** Date-range presets per PRD FR-7; day boundaries are local midnight. */
export const RANGE_PRESETS = ["day", "week", "month", "all"] as const;
export type RangePreset = (typeof RANGE_PRESETS)[number];

/** A preset, or the custom date window held in `customStart`/`customEnd`. */
export type RangeChoice = RangePreset | "custom";

export const RANGE_LABELS: Record<RangeChoice, string> = {
  day: "Today",
  week: "Last 7 days",
  month: "Last 30 days",
  all: "All time",
  custom: "Custom range",
};

/** Request origin: main conversation vs subagent (sidechain) traffic. */
export const QUERY_SOURCES = ["all", "main", "subagent"] as const;
export type QuerySource = (typeof QUERY_SOURCES)[number];

export const QUERY_SOURCE_LABELS: Record<QuerySource, string> = {
  all: "All sources",
  main: "Main only",
  subagent: "Subagents only",
};

/**
 * Project-filter sentinel for the unknown-project bucket (sessions with no
 * cwd mapping). Safe as a sentinel: real cwd values are absolute paths.
 */
export const UNKNOWN_PROJECT_OPTION = "(unknown project)";

export interface FacetSelection {
  /** Project (session cwd) filter; empty string = all projects,
   * [`UNKNOWN_PROJECT_OPTION`] = the unknown-project bucket. */
  project: string;
  /** Model filter; empty string = all models. */
  model: string;
  range: RangeChoice;
  /** Custom-range bounds (local dates, `yyyy-mm-dd`, both inclusive);
   * applied only while `range === "custom"`. */
  customStart: string;
  customEnd: string;
  querySource: QuerySource;
}

export const DEFAULT_FACETS: Readonly<FacetSelection> = {
  project: "",
  model: "",
  range: "month",
  customStart: "",
  customEnd: "",
  querySource: "all",
};

/** The shared selection. Mutate fields directly (or via the FacetBar). */
export const facets = $state<FacetSelection>({ ...DEFAULT_FACETS });

export function clearFacets(): void {
  Object.assign(facets, DEFAULT_FACETS);
}

/** Number of facets differing from the default (drives the Clear button). */
export function activeFacetCount(): number {
  return (
    (facets.range !== DEFAULT_FACETS.range ? 1 : 0) +
    (facets.querySource !== DEFAULT_FACETS.querySource ? 1 : 0) +
    (facets.project.trim() !== "" ? 1 : 0) +
    (facets.model.trim() !== "" ? 1 : 0)
  );
}

/** A `Date` as a local `yyyy-mm-dd` string (the date-input value format). */
export function localIsoDate(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
