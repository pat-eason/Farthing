// Self-contained report generation (Unit 5). These are the agreed signatures
// the view integrations (Units 6-7) and the export orchestrator (Unit 4) build
// against; the bodies are fleshed out in Unit 5.
//
// All functions here are PURE (string in, string out) so a future vitest setup
// can cover them without a runtime, and so the export command can write the
// returned strings verbatim into the bundle (R6/R7/R9).
//
// STORED-XSS CONTRACT (mirrors src-tauri/src/export.rs): the HTML these emit is
// written verbatim into a file the recipient opens in a browser, so the
// template MUST embed a restrictive `<meta http-equiv="Content-Security-Policy">`
// (no inline/remote script) and MUST NOT emit any `<script>` element. The chart
// is inline SVG with inline styles, so no script is ever needed to render it.

import type { ChartBucket } from "$lib/StackedBarChart.svelte";

/** One row of the aggregated CSV (R7): a header cell list plus the matching
 * value cells. Views supply rows in display order; the serializer quotes. */
export interface AggregatedCsv {
  /** Column headers, in order. */
  columns: string[];
  /** Data rows; each row has one cell per column (already stringified). */
  rows: string[][];
}

/** A filter chip shown in the report identity header (R6). */
export interface ReportFilter {
  label: string;
  value: string;
}

/** Exact (unrounded) totals embedded as data alongside display strings so the
 * standalone report's reconciliation holds (R9). */
export interface ReportTotals {
  /** Unrounded cost sum (unpriced rows as 0). */
  costUsd: number;
  /** api_request rows (errors excluded). */
  requests: number;
  /** api_request rows with no model pricing (cost excluded, tokens counted). */
  unpricedRequests: number;
  /** api_error rows in the window. */
  errors: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

/** Everything `buildReportHtml` needs to render a standalone report. The chart
 * is optional: table-based views (sessions, projects) pass `chartSvg`
 * undefined; chart views (cost, tokens) pass a prebuilt inline SVG string. */
export interface ReportInput {
  /** View display name, e.g. "Cost over time" (R6 identity header). */
  title: string;
  /** Resolved window label, e.g. "Jun 5 – Jun 12, 2026" (R6). */
  rangeLabel: string;
  /** Active filter chips (source, project, model, grouping where applicable). */
  filters: ReportFilter[];
  totals: ReportTotals;
  /** Inline-SVG chart markup (chart views) or undefined (table views). */
  chartSvg?: string;
  /** The aggregated table/CSV datapoints (R7). */
  aggregated: AggregatedCsv;
  /** Generated-on timestamp (unix ms); defaults to now in Unit 5. */
  generatedAtMs: number;
}

/** Serialize the aggregated datapoints to a CSV string (R7). One row per
 * chart/table datapoint; RFC-4180 quoting. */
export function buildSummaryCsv(aggregated: AggregatedCsv): string {
  // TODO(Unit 5): emit the header + one quoted row per datapoint.
  void aggregated;
  throw new Error("buildSummaryCsv not implemented (Unit 5)");
}

/** Build the standalone, self-contained `report.html` string (R6/R9): CSP meta,
 * inline CSS, identity header, totals band, the inline-SVG chart or table, and
 * the aggregated table. No external assets, no `<script>`. */
export function buildReportHtml(input: ReportInput): string {
  // TODO(Unit 5): assemble the document from the input.
  void input;
  throw new Error("buildReportHtml not implemented (Unit 5)");
}

/** Convenience re-export so chart views can type the buckets they pass through
 * to `chartSvg`'s builder without importing the component directly. */
export type { ChartBucket };
