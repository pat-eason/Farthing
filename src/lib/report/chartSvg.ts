// Standalone stacked-bar SVG generation (Unit 5). Regenerates a self-contained
// SVG from the snapshotted, point-in-time buckets + legend/PALETTE the page uses
// (NOT a serialization of the live DOM - see the plan's Key Technical
// Decisions), with inline `fill`/styles so the file renders with the app closed.
//
// PURE: buckets + legend in, SVG markup string out. No `<script>`, no external
// assets (the stored-XSS contract in buildReport.ts applies to the embedded
// SVG too).

import type { ChartBucket } from "$lib/StackedBarChart.svelte";
import { formatDate } from "$lib/format";

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

// Geometry mirrors StackedBarChart.svelte (the on-screen chart) so the report's
// bars look identical. The on-screen chart uses `preserveAspectRatio="none"`
// and CSS sizing; the standalone file has no stylesheet sizing it, so we render
// at a fixed pixel canvas and scale the unit-wide bar geometry into it.
const VIEW_HEIGHT = 100;
const TOP_PAD = 4;
const BASELINE = 1;
/** Rendered canvas; the document CSS caps the width responsively. */
const CANVAS_HEIGHT = 220;

/** Escape text for an XML/SVG text node or attribute value. */
function escapeXml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

const bucketTotal = (bucket: ChartBucket): number =>
  bucket.segments.reduce((sum, segment) => sum + segment.value, 0);

/**
 * Build a standalone inline-SVG stacked bar chart (with axis labels, peak
 * caption, and legend) that renders with the app closed (R6). Mirrors the
 * StackedBarChart scaling/stacking exactly so the report reproduces the
 * on-screen chart; emits inline `fill`/styles only (no `<script>`, no CSS
 * classes the standalone file couldn't resolve).
 */
export function buildChartSvg(input: ChartSvgInput): string {
  const { buckets, legend, formatValue, ariaLabel } = input;

  const maxTotal = buckets.reduce((max, bucket) => Math.max(max, bucketTotal(bucket)), 0);
  const usableHeight = VIEW_HEIGHT - TOP_PAD - BASELINE;
  const scaled = (value: number): number =>
    maxTotal <= 0 || value <= 0 ? 0 : (value / maxTotal) * usableHeight;

  // viewBox width = bucket count (one unit per bar), matching the on-screen
  // chart; bars are unit-wide with a small gutter (0.08 each side).
  const width = Math.max(buckets.length, 1);

  const parts: string[] = [];
  // Baseline spanning the full width, present even with no data.
  parts.push(
    `<rect x="0" y="${VIEW_HEIGHT - BASELINE}" width="${width}" height="${BASELINE}" fill="rgba(0,0,0,0.18)" />`
  );

  for (let i = 0; i < buckets.length; i++) {
    const bucket = buckets[i];
    // Stack offsets, bottom-up in segment order (same as StackedBarChart).
    let bottom = VIEW_HEIGHT - BASELINE;
    for (const segment of bucket.segments) {
      const h = scaled(segment.value);
      bottom -= h;
      if (segment.value > 0) {
        parts.push(
          `<rect x="${(i + 0.08).toFixed(4)}" y="${bottom.toFixed(4)}" width="0.84" height="${h.toFixed(4)}" fill="${escapeXml(segment.color)}" />`
        );
      }
    }
  }

  // `preserveAspectRatio="none"` lets the unit-geometry stretch to the canvas
  // width, exactly as the on-screen chart does.
  const svg =
    `<svg class="report-chart-svg" viewBox="0 0 ${width} ${VIEW_HEIGHT}" ` +
    `preserveAspectRatio="none" height="${CANVAS_HEIGHT}" ` +
    `role="img" aria-label="${escapeXml(ariaLabel)}" ` +
    `xmlns="http://www.w3.org/2000/svg">${parts.join("")}</svg>`;

  // Peak caption + axis edges as HTML siblings (the on-screen chart renders
  // these outside the SVG too).
  const peak =
    maxTotal > 0
      ? `<p class="report-chart-peak">peak day ${escapeXml(formatValue(maxTotal))}</p>`
      : "";
  const axis =
    buckets.length > 0
      ? `<div class="report-chart-axis"><span>${escapeXml(formatDate(buckets[0].start_ms))}</span>` +
        `<span>${escapeXml(formatDate(buckets[buckets.length - 1].start_ms))}</span></div>`
      : "";

  const legendHtml =
    legend.length > 0
      ? `<ul class="report-legend">` +
        legend
          .map(
            (entry) =>
              `<li><span class="report-swatch" style="background:${escapeXml(entry.color)}"></span>` +
              `<span>${escapeXml(entry.label)}</span></li>`
          )
          .join("") +
        `</ul>`
      : "";

  return `<figure class="report-chart">${peak}${svg}${axis}${legendHtml}</figure>`;
}
