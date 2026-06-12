---
date: 2026-06-12
topic: report-export
---

# Report Export (Spend Export Bundles)

## Problem Frame

Farthing visualizes spend across four report views (`cost`, `sessions`, `tokens`, `projects`), all driven by a shared facet selection (range, source, project, model). What it can't do today is get that data *out*: there's no way to hand a stakeholder a "here's our Claude spend for the last 7 days, by model" artifact, archive a month for the books, or pull the underlying rows into a spreadsheet for analysis Farthing doesn't do.

The goal is a per-view **export bundle**: from any report view, the user exports the currently-filtered view as a `.zip` containing a beautified, self-contained HTML report that reproduces what's on screen, plus CSVs of the data behind it. The export is a faithful snapshot of one view under its current filters: change nothing in planning that lets the filename or contents drift from "exactly what I was looking at when I clicked export."

The aggregated CSV and HTML reuse the existing query layer (`src-tauri/src/queries.rs`) and do not change how cost or usage is computed. The **raw CSV is the exception**: no existing command returns unbounded, window-scoped request rows (`session_detail` is session-scoped and hard-capped at 1000 rows; everything else is pre-aggregated). A new streaming raw-row query is required — the single largest new build in this feature, not a reuse of existing queries. See Open Questions.

## Decisions Resolved in Brainstorm

- **CSV = both aggregated and raw.** The bundle contains two CSVs: an *aggregated* CSV whose rows are exactly the chart/table datapoints (e.g. day × model for Cost-by-model), and a *raw* CSV of every underlying `api_request` row in the filtered window. The aggregated CSV answers "what built this chart"; the raw CSV is for the user's own analysis.
- **HTML reproduces the visual.** The report embeds the actual chart (inline SVG) the user is looking at — including their current toggle state (Total / By model / By project) — alongside headline totals and the aggregated data table. Not a generic summary.
- **"Currently filtered view" = full filtered dataset, not the on-screen page.** Sessions is paginated with "Load more"; the export covers every session matching the filters, not just visible rows. On-screen *grouping and sort* state does carry into the report.
- **Snapshot at click time, captured synchronously.** The click handler captures facet state, toggle/sort state, and a serialization of the rendered chart *synchronously, before any `await`* — so toggling the chart (e.g. By model → By project) or changing filters immediately after clicking cannot make the bundle's chart and CSVs disagree. The CSV data is queried from the snapshotted facets, never re-read from live mutable state.
- **All four views in v1.** The export framework (zip assembly, save dialog, progress, notification) is built once; each view plugs in its own HTML/CSV producers.
- **Raw CSV stays in v1, with home-relativized paths.** The raw CSV is included despite being the largest build, but cwd paths are home-stripped (`~/Projects/...` via the existing `home_dir` helper) so the bundle doesn't leak absolute filesystem paths when shared. Session IDs are retained for drill-in fidelity.
- **One global, serialized export at a time.** A single app-level banner persists across view navigation; the Export action disables while an export is in flight. No concurrent exports.
- **Notification earns its interruption.** The desktop notification fires only when the export ran longer than a threshold (e.g. > 2s) or the user navigated away from the originating view; fast exports surface completion only in the in-app banner.

## Requirements

**Trigger and scope**
- R1. Each of the four report views (`cost`, `sessions`, `tokens`, `projects`) has an "Export" action in its view header.
- R2. Clicking Export captures a snapshot of the current facet selection and the current per-view toggle/sort state, then opens a native save dialog for the user to choose the `.zip` destination.
- R3. The default filename encodes the view, range, and active facets so it's self-describing (e.g. `farthing-cost-last7days-main-by-model.zip`; exact format deferred to planning).
- R4. The bundle reflects the **full filtered dataset**, not the paginated/visible window. On-screen grouping (By model / By project) and sort order carry into the report.

**Bundle contents**
- R5. The `.zip` contains: one HTML report, one aggregated CSV, and one raw-rows CSV.
- R6. The **HTML report** is a single self-contained file (inline CSS, inline SVG — no external assets or network dependencies) that states the report identity: view name (e.g. "Cost over time"), resolved date window, source filter, project/model filters (where the view supports them), and grouping (where applicable). It embeds the chart **or table** the user is viewing (chart for `cost`/`tokens`, table for `sessions`/`projects`) plus headline totals and the aggregated data table. Self-containment is non-trivial for charts: the on-screen SVG is styled by page-scoped CSS and surrounded by sibling HTML — axis labels, peak caption, legend, dark/light variants — that live *outside* the `<svg>` element. The report must inline those computed styles and reconstruct the legend/axis, not just serialize the `<svg>` node.
- R7. The **aggregated CSV** has one row per chart/table datapoint — the exact data that produced the visual (e.g. one row per day × model, or one row per session rollup, or one row per project).
- R8. The **raw CSV** has one row per underlying request row in the filtered window, with the request-level columns: timestamp, model, query_source, event_type, source, cost, the four token kinds (input, output, cache_read, cache_creation), duration, and error. Any `cwd`/path column is home-relativized (`~/...`) so the bundle does not leak absolute filesystem paths when shared. Whether `api_error` rows (null cost/model) are included is a column-set decision tied to R9: if included, a raw row *count* will exceed the aggregated `requests` total, which counts only `api_request` rows.
- R9. The HTML and both CSVs reconcile **against unrounded numeric values** — on-screen HTML totals are display-formatted/rounded by `formatCost` (`format.ts`), so the report carries exact values for the check. Reconciliation is defined over additive columns: cost reconciles treating unpriced rows (`cost_usd` NULL) as 0; each aggregated row's totals equal the sum of its matching raw rows (grouped by the view's key, e.g. day × model). Non-additive derived columns (session duration, distinct-session counts) are excluded. The bundle surfaces unpriced-request and error counts so a raw cost sum lower than token volume implies reads as explained, not as a bug.

**Export feedback (progress → completion)**
- R10. While the export runs, a single non-blocking, app-level banner shows status with a progress bar and persists across view navigation. The user can keep navigating; the banner does not modal-lock the UI. Exports are serialized: the Export action is disabled while an export is in flight, so there is never more than one in-flight export or more than one banner.
- R11. The progress bar reflects real work. The raw-rows CSV on a wide range (e.g. All time) can be very large; progress should track the dominant cost (row streaming / file write), not be a fixed animation.
- R12. On success, the banner shows a completion state and the user is told where the file was saved with an affordance to reveal it (e.g. "Show in Finder"). A desktop notification fires only when the export ran longer than a threshold (e.g. > 2s) or the user navigated away from the originating view — fast exports surface completion only in the banner, to keep notifications meaningful. The notification is best-effort: if permission is denied, the in-app banner is the primary signal and no error is shown for the missing notification. Confirm during planning that the `opener` plugin can *reveal* a file in Finder, not only open it; if not, this needs another mechanism.
- R13. On failure (dialog cancelled, write error, permission denied), the export aborts cleanly: the banner shows an actionable error, no partial `.zip` is left behind (write to a temp path and atomically rename to the chosen destination only on full success; remove the temp on any failure), no success notification fires, and the Export action returns to its normal enabled state so the user can retry.

**Per-view fidelity**
- R14. `cost`: embeds the stacked bar chart in its current grouping (Total / By model / By project); aggregated CSV is the series data backing it (one row per day × group).
- R15. `tokens`: embeds the token/cache charts (state explicitly during planning whether tokens supports the same grouping toggle as cost or none); aggregated CSV is the per-bucket token series.
- R16. `sessions`: table-based report in current sort order; aggregated CSV is the full set of session rollups (all pages). The session aggregation excludes session-less rows (`session_id IS NULL`); the raw CSV for this view must use the same row set so R9 reconciliation holds, written in a deterministic order (session_id, then timestamp) — the session-level sort key has no meaning per request row.
- R17. `projects`: table-based report with cost-share; aggregated CSV is the per-project rollups.

**Interaction states**
- R18. Empty filtered window: if the snapshot matches zero rows, the export is guarded *before* the save dialog opens — the Export action surfaces a non-error "nothing matches these filters" message rather than producing an empty bundle.

## Bundle Layout

```
farthing-cost-last7days-main-by-model.zip
├── report.html        # self-contained: identity header + inline-SVG chart + totals + aggregated table
├── summary.csv         # aggregated rows — exactly what built the chart/table
└── requests.csv        # raw api_request rows in the filtered window
```

## Out of Scope (v1)

- Scheduled / recurring exports or auto-export.
- Cloud upload, email, or share-link delivery (local file only).
- PDF or formats other than HTML + CSV.
- Cross-view "export everything" bundles (export is per-view).
- Editing/customizing report styling or column selection.

## Open Questions for Planning

- **New raw-row query command (load-bearing):** design a faceted, window-scoped, unbounded streaming reader over the `requests` table that reuses the existing `Facets`/`FacetFilter` machinery. Decide return shape (Vec vs row-callback/iterator) since R11 progress and memory safety both depend on it. State whether the deliberate 1000-row `session_detail` cap is intentionally bypassed.
- **DB concurrency under a long export:** the connection is `Arc<Mutex<Db>>`, shared with the live OTLP ingest task; a long All-time scan holding that lock would stall ingest and freeze other UI queries, contradicting R10's "keep navigating." Options: open a second read-only connection for export (WAL allows concurrent readers) or chunk the scan with bounded windows that release the lock between chunks. Lean second read connection.
- **Streaming + memory:** stream raw rows incrementally to a temp CSV inside the zip (never materialize all rows or build the zip in memory); validate behavior at ~1M rows. Decide an explicit upper bound or confirm "no cap, streamed."
- **Plugin + capabilities additions:** add `tauri-plugin-dialog` and `tauri-plugin-notification` (verified absent today; only `opener`, `positioner`, `autostart` installed) and their permission identifiers (e.g. `dialog:default`, `notification:default`) to `src-tauri/capabilities/default.json`. Zip assembly stays in Rust (`std::fs` + the `zip` crate — not currently in the dependency tree).
- **Chart capture mechanism:** serialize the live-DOM SVG at click time (faithful, captures toggle state) vs. regenerate the chart in the report from the snapshotted series. Either way, R6 self-containment requires inlining computed styles and reconstructing the legend, axis labels, peak caption, and dark/light variants, which live *outside* the `<svg>` element in scoped CSS and sibling HTML. Smoke test: `report.html` opens in a browser with the app not running and renders the full chart, not just the bars.
- **Progress granularity:** how progress is reported from Rust to the banner (event stream / polling); the dominant-cost unit per view (row count vs bytes); and the duration/row threshold below which the operation is effectively instant (e.g. < ~300ms uses no real progress bar).
- **Default filename format** and filename-safe encoding of facets into it (custom ranges, `~`/absolute-path projects, arbitrary model names; collision behavior on repeat exports — a timestamp would disambiguate).

## Success Criteria

- From any of the four views, Export produces a `.zip` whose HTML opens standalone in a browser (with the app not running) with the chart/table fully intact — axis labels, legend, baseline, peak caption, and dark/light rendering, not just the bars — no broken assets, and an identity header matching the filters that were active.
- The aggregated CSV reproduces the chart datapoints exactly; the raw CSV's exact (unrounded) cost/token sums reconcile with the report totals, treating unpriced rows as zero cost and accounting for excluded `api_error` rows.
- A large export (All time, raw rows) shows a progress bar that visibly advances and never blocks navigation; a small export completes near-instantly without a fake delay.
- On completion the user gets both an in-app completion state and a desktop notification, and can reveal the saved file in one click.
- Cancelling the save dialog or hitting a write error leaves no partial file and surfaces a clear, actionable message.
