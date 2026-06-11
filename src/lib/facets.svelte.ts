// Global facet selections (task 5.1): one shared, mutable state object that
// every analysis view reads, so navigating between views never resets the
// active filters. Module-level $state lives for the webview's lifetime; the
// main window is hidden on close, never destroyed, so selections also
// survive close/reopen of the desktop window within an app run.
//
// Task 5.2 turns these selections into SQL facet params; tasks 5.3-5.6 feed
// them to their aggregation commands. Until then, project and model are
// free-text filters (5.2's facet layer provides the real option lists).

/** Date-range presets per PRD FR-7; day boundaries are local midnight. */
export const RANGE_PRESETS = ["day", "week", "month", "all"] as const;
export type RangePreset = (typeof RANGE_PRESETS)[number];

export const RANGE_LABELS: Record<RangePreset, string> = {
  day: "Today",
  week: "Last 7 days",
  month: "Last 30 days",
  all: "All time",
};

/** Request origin: main conversation vs subagent (sidechain) traffic. */
export const QUERY_SOURCES = ["all", "main", "subagent"] as const;
export type QuerySource = (typeof QUERY_SOURCES)[number];

export const QUERY_SOURCE_LABELS: Record<QuerySource, string> = {
  all: "All sources",
  main: "Main only",
  subagent: "Subagents only",
};

export interface FacetSelection {
  /** Project (session cwd) filter; empty string = all projects. */
  project: string;
  /** Model filter; empty string = all models. */
  model: string;
  range: RangePreset;
  querySource: QuerySource;
}

export const DEFAULT_FACETS: Readonly<FacetSelection> = {
  project: "",
  model: "",
  range: "month",
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
