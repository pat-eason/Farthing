// Shared, view-agnostic export machinery (Unit 4): one module-level `$state`
// the app-level banner reads (mirroring facets.svelte.ts), the `runExport`
// orchestrator, and the pure default-filename builder.
//
// One global serialized export: `runExport` sets `status` to a non-idle value
// synchronously as its first statement, so a second Export click from any view
// is blocked before the first awaits the dialog (closes the click-window race).
// Views disable their Export button via `isExporting()`.
//
// Data flow (per the plan): the *view* supplies a `prepare` callback that
// performs the consistent read (existing query commands) and builds the
// report HTML + aggregated CSV (Unit 5 functions); `runExport` owns the empty
// guard, the filename, the save dialog, the export invoke, and the
// notification/reveal decisions. Keeping report-building in the view keeps
// `runExport` view-agnostic, and keeps the filename builder + guard pure.

import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { page } from "$app/state";
import { notifyExportDone, runExportCommand, type Facets, type RangeFacet } from "$lib/queries";

/** The four exportable views; doubles as the filename `<view>` segment. */
export type ExportView = "cost" | "sessions" | "tokens" | "projects";

/** Banner state machine (R10/R12/R13/R18):
 * - `idle`: nothing in flight; Export enabled.
 * - `preparing`: consistent read + report build + dialog open (pre-write).
 * - `working`: Rust is streaming/zipping; the progress bar tracks real work.
 * - `done`: bundle written; banner offers Show-in-Finder.
 * - `error`: clean abort; actionable message; Export re-enabled.
 * - `guarded`: empty filtered window; transient non-error notice (R18). */
export type ExportStatus = "idle" | "preparing" | "working" | "done" | "error" | "guarded";

/** Auto-clear timers (ms). `done` lingers so the user can hit Show-in-Finder;
 * the transient states clear themselves so the banner never gets stuck. */
const DONE_AUTO_DISMISS_MS = 8000;
const GUARDED_AUTO_CLEAR_MS = 4000;
const ERROR_AUTO_CLEAR_MS = 8000;
/** Notification fires when an export took longer than this (R12). */
const SLOW_EXPORT_MS = 2000;

/** What a view's `prepare` returns: the consistent-read counts that drive the
 * empty guard + progress denominator, plus the frontend-rendered bundle inputs
 * and the per-view raw-CSV flag. */
export interface PreparedExport {
  /** api_request rows in the window (errors excluded). */
  requests: number;
  /** api_error rows in the window. Empty guard: `requests === 0 && errors === 0`. */
  errors: number;
  /** Self-contained `report.html` string (Unit 5). */
  reportHtml: string;
  /** Aggregated `summary.csv` string (Unit 5). */
  summaryCsv: string;
  /** Sessions view passes `true` (raw CSV = session-rollup set, R16); the other
   * three pass `false`. */
  excludeSessionless: boolean;
  /** Active grouping for the filename (e.g. "model"); omit when ungrouped or
   * the view has no grouping toggle. */
  grouping?: string;
}

/** Arguments for `runExport`. The view captures `facets`/`originRoute`
 * synchronously on click, then hands the consistent read + report build to
 * `prepare`. */
export interface RunExportOptions {
  view: ExportView;
  /** Resolved SQL facets — the identical window Rust reads the raw rows from. */
  facets: Facets;
  /** Route the export was triggered from (e.g. "/cost"), for the navigation
   * notification check. */
  originRoute: string;
  /** Consistent read + report build, supplied by the view. */
  prepare: () => Promise<PreparedExport>;
}

/** The shared banner state. Mutate fields directly (only `runExport` and the
 * progress listener should). */
export const exportState = $state<{
  status: ExportStatus;
  /** Rows streamed so far (R11). */
  rowsWritten: number;
  /** `requests + errors` denominator; 0 until prepare completes. */
  totalRows: number;
  /** Absolute path of the written bundle, for Show-in-Finder. */
  savedPath: string | undefined;
  /** Route the active export was triggered from. */
  originRoute: string | undefined;
  /** Banner copy: guard notice, error detail, or success summary. */
  message: string;
}>({
  status: "idle",
  rowsWritten: 0,
  totalRows: 0,
  savedPath: undefined,
  originRoute: undefined,
  message: "",
});

/** Pending auto-clear timer so re-entrancy never leaves two timers racing. */
let clearTimer: ReturnType<typeof setTimeout> | undefined;
/** Guards the single-notification rule against a double-fire. */
let notified = false;

function cancelClearTimer(): void {
  if (clearTimer !== undefined) {
    clearTimeout(clearTimer);
    clearTimer = undefined;
  }
}

/** Reset to idle and clear transient fields. Safe to call from any state. */
export function resetExport(): void {
  cancelClearTimer();
  exportState.status = "idle";
  exportState.rowsWritten = 0;
  exportState.totalRows = 0;
  exportState.savedPath = undefined;
  exportState.originRoute = undefined;
  exportState.message = "";
  notified = false;
}

/** True whenever an export is in any non-idle phase; views bind their Export
 * button's `disabled` to this so exports are serialized (R10) and the button
 * leaves the tab order while busy. */
export function isExporting(): boolean {
  return exportState.status !== "idle";
}

// ---- pure helpers (kept pure for future vitest coverage) ----

/** A facet range as a filename-safe segment, e.g. "day" -> "today",
 * a custom window -> "custom". */
function rangeSegment(range: RangeFacet | undefined): string {
  if (range === undefined) return "all";
  if (typeof range === "string") {
    // Map the preset keys to readable slugs the recipient understands.
    return { day: "today", week: "7d", month: "30d", all: "all-time" }[range] ?? range;
  }
  return "custom";
}

/** A facet query-source as a filename segment, or undefined when "all"
 * (the default carries no segment). */
function sourceSegment(querySource: Facets["query_source"]): string | undefined {
  if (querySource === undefined || querySource === "all") return undefined;
  return querySource;
}

/** Slugify a segment for a filename: lowercase, non-alphanumerics to single
 * hyphens, trimmed, and length-bounded so a long cwd can't blow up the name. */
export function slugifySegment(value: string, maxLength = 24): string {
  const slug = value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug.slice(0, maxLength).replace(/-+$/g, "");
}

/** Local `YYYY-MM-DD` for the filename date stamp (also disambiguates repeats).
 * Takes the instant as `nowMs` (defaulting to now) rather than a `Date` so this
 * module stays free of mutable `Date` state. */
function localDateStamp(nowMs: number): string {
  const d = new Date(nowMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** Build the default save filename (R3):
 * `farthing-<view>-<YYYY-MM-DD>-<range>[-<source>][-<grouping>].zip`.
 * Facet segments are slugified and length-bounded. Pure (the date stamp is
 * derived from `nowMs`, defaulting to the call instant). */
export function buildDefaultFilename(
  view: ExportView,
  facets: Facets,
  grouping: string | undefined,
  nowMs: number = Date.now()
): string {
  const parts = [
    "farthing",
    view,
    localDateStamp(nowMs),
    slugifySegment(rangeSegment(facets.range)),
  ];
  const source = sourceSegment(facets.query_source);
  if (source) parts.push(slugifySegment(source));
  if (grouping && grouping !== "none") parts.push(slugifySegment(grouping));
  return `${parts.filter((p) => p !== "").join("-")}.zip`;
}

// ---- orchestration ----

/** Schedule the banner to auto-clear back to idle after `delay`, unless a new
 * export supersedes it first. */
function scheduleClear(delay: number): void {
  cancelClearTimer();
  clearTimer = setTimeout(() => {
    clearTimer = undefined;
    resetExport();
  }, delay);
}

/** Fire the completion notification at most once (R12): only when the export
 * was slow OR the user has navigated away from the originating route. Swallows
 * any plugin error (denied permission, plugin unavailable) — the banner is the
 * primary success surface. */
async function maybeNotify(view: ExportView, elapsedMs: number): Promise<void> {
  if (notified) return;
  const navigatedAway =
    exportState.originRoute !== undefined && page.url.pathname !== exportState.originRoute;
  if (elapsedMs <= SLOW_EXPORT_MS && !navigatedAway) return;
  notified = true;
  try {
    await notifyExportDone("Export complete", `Your ${view} report is ready.`);
  } catch {
    // Best-effort; the banner already shows completion.
  }
}

/**
 * Orchestrate one export end-to-end (R2/R3/R10/R11/R12/R13/R18).
 *
 * Sets `status="preparing"` synchronously as the first statement so a second
 * Export click (from any view) sees a non-idle status and is blocked before the
 * dialog `await`. Flow: prepare (consistent read + report build) -> empty guard
 * -> save dialog -> invoke -> the progress listener flips to `done`/`error`.
 */
export async function runExport(options: RunExportOptions): Promise<void> {
  // Serialized single export: a second concurrent call is a no-op.
  if (isExporting()) return;

  // FIRST statement, synchronous: closes the click-window race.
  cancelClearTimer();
  notified = false;
  exportState.status = "preparing";
  exportState.rowsWritten = 0;
  exportState.totalRows = 0;
  exportState.savedPath = undefined;
  exportState.originRoute = options.originRoute;
  exportState.message = "";

  let prepared: PreparedExport;
  try {
    prepared = await options.prepare();
  } catch (err) {
    exportState.status = "error";
    exportState.message = `Couldn't prepare the export: ${String(err)}`;
    scheduleClear(ERROR_AUTO_CLEAR_MS);
    return;
  }

  // Empty guard (R18): a window with no requests AND no errors has nothing to
  // export — show a transient notice and never open the dialog.
  if (prepared.requests === 0 && prepared.errors === 0) {
    exportState.status = "guarded";
    exportState.message = "No data matches the current filters";
    scheduleClear(GUARDED_AUTO_CLEAR_MS);
    return;
  }

  const totalRows = prepared.requests + prepared.errors;
  exportState.totalRows = totalRows;

  let destination: string | null;
  try {
    destination = await save({
      defaultPath: buildDefaultFilename(options.view, options.facets, prepared.grouping),
      filters: [{ name: "Zip archive", extensions: ["zip"] }],
    });
  } catch (err) {
    exportState.status = "error";
    exportState.message = `Couldn't open the save dialog: ${String(err)}`;
    scheduleClear(ERROR_AUTO_CLEAR_MS);
    return;
  }

  // Cancelled dialog: back to idle, no error, button re-enabled.
  if (destination === null) {
    resetExport();
    return;
  }

  exportState.status = "working";
  exportState.rowsWritten = 0;

  let result;
  try {
    result = await runExportCommand({
      destination,
      facets: options.facets,
      reportHtml: prepared.reportHtml,
      summaryCsv: prepared.summaryCsv,
      totalRows,
      excludeSessionless: prepared.excludeSessionless,
    });
  } catch (err) {
    // Rust aborts atomically (no partial .zip); surface the message (R13).
    exportState.status = "error";
    exportState.message = `Export failed: ${String(err)}`;
    scheduleClear(ERROR_AUTO_CLEAR_MS);
    return;
  }

  // The terminal `done` progress event may also arrive via the listener; both
  // paths converge on the same state, and the listener's `done` is idempotent.
  exportState.status = "done";
  exportState.savedPath = destination;
  exportState.rowsWritten = result.rowsWritten;
  exportState.message = "Export complete";
  scheduleClear(DONE_AUTO_DISMISS_MS);

  void maybeNotify(options.view, result.elapsedMs);
}

/** Reveal the saved bundle in Finder (R12). Best-effort: a moved/deleted file
 * surfaces a brief note rather than throwing. Dismisses the banner immediately
 * (the user acted on it). */
export async function revealSavedExport(): Promise<void> {
  const path = exportState.savedPath;
  if (path === undefined) return;
  try {
    await revealItemInDir(path);
    resetExport();
  } catch {
    exportState.message = `That file is no longer at ${path}`;
  }
}

/** Apply a streamed progress event to the banner (R11). Called by the layout's
 * listener. `writing` advances the bar; `done` flips to the completion state
 * (idempotent with `runExport`'s own `done` transition). Progress is clamped to
 * <=100% defensively. */
export function applyProgress(payload: {
  phase: string;
  rowsWritten: number;
  totalRows: number;
}): void {
  // Ignore stray events when no export is active (idle/guarded/error).
  if (exportState.status !== "working" && exportState.status !== "done") return;
  if (payload.totalRows > 0) exportState.totalRows = payload.totalRows;
  exportState.rowsWritten = Math.min(
    payload.rowsWritten,
    exportState.totalRows || payload.rowsWritten
  );
  if (payload.phase === "done" && exportState.status === "working") {
    exportState.status = "done";
    exportState.message = "Export complete";
    scheduleClear(DONE_AUTO_DISMISS_MS);
  }
}

/** Fraction [0,1] for the progress bar; 0 when the total is unknown. */
export function exportProgressFraction(): number {
  if (exportState.totalRows <= 0) return 0;
  return Math.min(exportState.rowsWritten / exportState.totalRows, 1);
}
