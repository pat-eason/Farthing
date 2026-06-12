// Self-contained report generation (Unit 5). These are the agreed signatures
// the view integrations (Units 6-7) and the export orchestrator (Unit 4) build
// against.
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
//
// DARK/LIGHT STRATEGY: fixed light appearance regardless of the viewer's OS.
// This is a handed-off, potentially printed/pasted-into-a-deck artifact, so a
// stable light document reads predictably everywhere (the plan recommends this).
// The PALETTE chart hexes are theme-independent and carry into the SVG verbatim.

import { formatCost, formatDate, formatTokens } from "$lib/format";
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

/** Restrictive policy for a standalone artifact: no script at all, no remote
 * fetches; inline styles only (the document and the chart are inline CSS/SVG).
 * Allowing `style-src 'unsafe-inline'` is required because the whole document
 * is inline-styled and has no external sheet; no `script-src` is granted, so
 * any injected `<script>` is inert. */
const CSP =
  "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'";

/** Escape text for an HTML text node or double-quoted attribute. */
function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Quote one CSV cell per RFC-4180: wrap in double-quotes and double any
 * embedded quote when the value carries a comma, quote, CR, or LF. */
function csvCell(value: string): string {
  if (/[",\r\n]/.test(value)) return `"${value.replace(/"/g, '""')}"`;
  return value;
}

/**
 * Serialize the aggregated datapoints to a CSV string (R7). One row per
 * chart/table datapoint; RFC-4180 quoting; CRLF line endings (the
 * spreadsheet-friendly default). Trailing newline so the last row is
 * well-terminated.
 */
export function buildSummaryCsv(aggregated: AggregatedCsv): string {
  const lines = [aggregated.columns, ...aggregated.rows].map((row) =>
    row.map(csvCell).join(",")
  );
  return lines.join("\r\n") + "\r\n";
}

/** A single trimmed numeric string (no exponent, full precision) for the
 * embedded reconciliation `data-*` attributes (R9). */
function exact(value: number): string {
  // Costs can be sub-cent; tokens/counts are integers. `String(number)` keeps
  // full IEEE-754 precision without forcing a fixed scale.
  return String(value);
}

/** The report stylesheet: a deliberate, neutral, print-friendly document look
 * (fixed light). Chart colors come from the inline SVG (PALETTE), so this only
 * styles the surrounding chrome + the aggregated table. */
function reportStyles(): string {
  return `
    :root { color-scheme: light; }
    * { box-sizing: border-box; }
    html, body { margin: 0; padding: 0; background: #f4f4f5; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      color: #1c1c1e;
      line-height: 1.45;
      -webkit-font-smoothing: antialiased;
    }
    .report {
      max-width: 60rem;
      margin: 0 auto;
      padding: 2rem 1.5rem 3rem;
    }
    .report-header { border-bottom: 1px solid #d8d8dc; padding-bottom: 1rem; }
    .report-title { margin: 0; font-size: 1.5rem; font-weight: 700; }
    .report-meta { margin: 0.35rem 0 0; font-size: 0.85rem; color: #6b6b6b; }
    .report-chips { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-top: 0.8rem; padding: 0; list-style: none; }
    .report-chip {
      display: inline-flex; gap: 0.3rem; align-items: baseline;
      padding: 0.2rem 0.55rem; border-radius: 999px;
      background: #e8e8ea; font-size: 0.75rem;
    }
    .report-chip b { font-weight: 600; }
    .report-chip span { color: #6b6b6b; }
    .report-totals {
      display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.5rem 1.2rem;
      margin: 1.4rem 0; padding: 1rem 1.2rem;
      background: #ffffff; border: 1px solid #e2e2e6; border-radius: 12px;
    }
    .report-total-cost { font-size: 1.9rem; font-weight: 700; font-variant-numeric: tabular-nums; }
    .report-total-item { font-size: 0.85rem; color: #6b6b6b; font-variant-numeric: tabular-nums; }
    .report-total-item b { color: #1c1c1e; font-weight: 600; }
    .report-footnote { margin: 0.6rem 0 0; font-size: 0.72rem; color: #6b6b6b; line-height: 1.4; }
    .report-chart {
      margin: 1.4rem 0; padding: 1.1rem 1.2rem;
      background: #ffffff; border: 1px solid #e2e2e6; border-radius: 12px;
    }
    .report-chart-svg { display: block; width: 100%; height: 220px; }
    .report-chart-peak { margin: 0 0 0.3rem; text-align: right; font-size: 0.72rem; color: #6b6b6b; font-variant-numeric: tabular-nums; }
    .report-chart-axis { display: flex; justify-content: space-between; margin-top: 0.3rem; font-size: 0.72rem; color: #6b6b6b; }
    .report-legend { list-style: none; display: flex; flex-wrap: wrap; gap: 0.35rem 1.1rem; margin: 0.9rem 0 0; padding: 0; font-size: 0.78rem; }
    .report-legend li { display: flex; align-items: center; gap: 0.4rem; }
    .report-swatch { width: 0.7rem; height: 0.7rem; border-radius: 3px; flex-shrink: 0; }
    .report-table-wrap { margin: 1.4rem 0 0; overflow-x: auto; }
    h2.report-section { font-size: 0.95rem; font-weight: 650; margin: 1.6rem 0 0.6rem; }
    table.report-table { border-collapse: collapse; width: 100%; font-size: 0.8rem; }
    table.report-table th, table.report-table td {
      text-align: left; padding: 0.45rem 0.7rem; border-bottom: 1px solid #ececef;
      font-variant-numeric: tabular-nums; white-space: nowrap;
    }
    table.report-table th { color: #6b6b6b; font-weight: 600; border-bottom-color: #d8d8dc; }
    table.report-table tr:last-child td { border-bottom: none; }
    .report-generated { margin-top: 2rem; font-size: 0.72rem; color: #9b9b9f; }
  `
    .replace(/\n\s+/g, "\n")
    .trim();
}

/** Render the identity-header filter chips (R6). */
function renderChips(filters: ReportFilter[]): string {
  if (filters.length === 0) return "";
  const items = filters
    .map(
      (f) =>
        `<li class="report-chip"><b>${escapeHtml(f.label)}</b> <span>${escapeHtml(f.value)}</span></li>`
    )
    .join("");
  return `<ul class="report-chips">${items}</ul>`;
}

/** Render the prominent totals band (R9): a display cost + the request / error /
 * unpriced / token counts, with the exact unrounded values embedded as
 * `data-*` so the standalone report reconciles against the raw CSV. */
function renderTotals(t: ReportTotals): string {
  const totalTokens =
    t.inputTokens + t.outputTokens + t.cacheReadTokens + t.cacheCreationTokens;
  const items: string[] = [
    `<span class="report-total-item">API-equivalent</span>`,
    `<span class="report-total-item"><b>${t.requests.toLocaleString()}</b> request${t.requests === 1 ? "" : "s"}</span>`,
    `<span class="report-total-item"><b>${formatTokens(totalTokens)}</b> tokens</span>`,
  ];
  if (t.errors > 0) {
    items.push(
      `<span class="report-total-item"><b>${t.errors.toLocaleString()}</b> error${t.errors === 1 ? "" : "s"}</span>`
    );
  }
  if (t.unpricedRequests > 0) {
    items.push(
      `<span class="report-total-item"><b>${t.unpricedRequests.toLocaleString()}</b> unpriced</span>`
    );
  }

  // Exact, unrounded values for reconciliation against the raw CSV (R9).
  const data =
    `data-cost-usd="${escapeHtml(exact(t.costUsd))}" ` +
    `data-requests="${escapeHtml(exact(t.requests))}" ` +
    `data-errors="${escapeHtml(exact(t.errors))}" ` +
    `data-unpriced-requests="${escapeHtml(exact(t.unpricedRequests))}" ` +
    `data-input-tokens="${escapeHtml(exact(t.inputTokens))}" ` +
    `data-output-tokens="${escapeHtml(exact(t.outputTokens))}" ` +
    `data-cache-read-tokens="${escapeHtml(exact(t.cacheReadTokens))}" ` +
    `data-cache-creation-tokens="${escapeHtml(exact(t.cacheCreationTokens))}"`;

  const footnote =
    t.unpricedRequests > 0
      ? `<p class="report-footnote">${t.unpricedRequests.toLocaleString()} request${t.unpricedRequests === 1 ? "" : "s"} with unknown pricing excluded from cost (tokens counted). ` +
        `The raw CSV may contain more rows than the request count above: error rows are included there and counted separately.</p>`
      : t.errors > 0
        ? `<p class="report-footnote">The raw CSV includes ${t.errors.toLocaleString()} error row${t.errors === 1 ? "" : "s"} counted separately from the request total above.</p>`
        : "";

  return (
    `<section class="report-totals" ${data}>` +
    `<span class="report-total-cost">${formatCost(t.costUsd)}</span>` +
    items.join("") +
    `</section>` +
    footnote
  );
}

/** Render the aggregated datapoints as a readable HTML table (R6); the exact
 * same rows serialized to `summary.csv` (R7). */
function renderTable(aggregated: AggregatedCsv): string {
  const head = aggregated.columns.map((c) => `<th>${escapeHtml(c)}</th>`).join("");
  const body = aggregated.rows
    .map((row) => `<tr>${row.map((cell) => `<td>${escapeHtml(cell)}</td>`).join("")}</tr>`)
    .join("");
  return (
    `<h2 class="report-section">Detail</h2>` +
    `<div class="report-table-wrap"><table class="report-table">` +
    `<thead><tr>${head}</tr></thead><tbody>${body}</tbody></table></div>`
  );
}

/**
 * Build the standalone, self-contained `report.html` string (R6/R9): CSP meta,
 * inline CSS, identity header, totals band, the inline-SVG chart or table, and
 * the aggregated table. No external assets, no `<script>`.
 */
export function buildReportHtml(input: ReportInput): string {
  const generated = formatDate(input.generatedAtMs);
  const chart = input.chartSvg ?? "";
  const meta = `${escapeHtml(input.rangeLabel)} · Farthing`;

  return (
    `<!DOCTYPE html>` +
    `<html lang="en">` +
    `<head>` +
    `<meta charset="utf-8" />` +
    `<meta http-equiv="Content-Security-Policy" content="${CSP}" />` +
    `<meta name="viewport" content="width=device-width, initial-scale=1" />` +
    `<title>${escapeHtml(input.title)} — Farthing report</title>` +
    `<style>${reportStyles()}</style>` +
    `</head>` +
    `<body>` +
    `<main class="report">` +
    `<header class="report-header">` +
    `<h1 class="report-title">${escapeHtml(input.title)}</h1>` +
    `<p class="report-meta">${meta}</p>` +
    renderChips(input.filters) +
    `</header>` +
    renderTotals(input.totals) +
    chart +
    renderTable(input.aggregated) +
    `<p class="report-generated">Generated ${escapeHtml(generated)} by Farthing</p>` +
    `</main>` +
    `</body>` +
    `</html>`
  );
}

/** Convenience re-export so chart views can type the buckets they pass through
 * to `chartSvg`'s builder without importing the component directly. */
export type { ChartBucket };
