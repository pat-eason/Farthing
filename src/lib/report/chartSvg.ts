// Standalone stacked-bar SVG generation (Unit 5). Regenerates a self-contained
// SVG from the snapshotted, point-in-time buckets + legend/PALETTE the page uses
// (NOT a serialization of the live DOM — see the plan's Key Technical
// Decisions), with inline `fill`/styles so the file renders with the app closed.
//
// PURE: buckets + legend in, SVG markup string out. No `<script>`, no external
// assets (the stored-XSS contract in buildReport.ts applies to the embedded
// SVG too).

import type { ChartBucket } from "$lib/StackedBarChart.svelte";

/** One legend entry in rank order; mirrors the page's legend derivation
 * (cost/+page.svelte) so the report's colors match the on-screen chart. */
export interface ChartLegendEntry {
  id: string;
  label: string;
  color: string;
}

/** Inputs for the standalone chart SVG. */
export interface ChartSvgInput {
  /** Per-day buckets with stack segments, bottom-up (same shape the on-screen
   * `StackedBarChart` consumes). */
  buckets: ChartBucket[];
  /** Legend entries in stack (rank) order, for the inline legend. */
  legend: ChartLegendEntry[];
  /** Formats a stacked value for the axis/peak caption (e.g. `formatCost`). */
  formatValue: (value: number) => string;
  /** Accessible chart description. */
  ariaLabel: string;
}

/** Build a standalone inline-SVG stacked bar chart (with axis labels, peak
 * caption, and legend) that renders with the app closed (R6). */
export function buildChartSvg(input: ChartSvgInput): string {
  // TODO(Unit 5): regenerate the SVG from buckets + legend with inline styles.
  void input;
  throw new Error("buildChartSvg not implemented (Unit 5)");
}
