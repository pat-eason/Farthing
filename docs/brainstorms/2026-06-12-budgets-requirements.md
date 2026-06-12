---
date: 2026-06-12
topic: budgets
---

# Budgets (Daily & Monthly Spend Targets)

## Problem Frame

Farthing shows today's live spend in the tray and a richer breakdown in the popover, but there is no notion of a *target* to measure that spend against. A glance tells you "$123 today" but not "is that a lot?" — there's no ceiling, no progress, no sense of pace.

The existing cost-notifications brainstorm (`docs/brainstorms/2026-06-12-cost-notifications-requirements.md`) defined a "monthly cap" as an alerting rule. But a cap and a budget are the same underlying number, and treating them as two separate configs invites drift. This feature makes **budgets a first-class primitive**: an optional daily amount and an optional monthly amount, defined once, then surfaced everywhere — the tray title, the popover (color-coded progress with countdown), and the desktop notifications the notifications brainstorm already specced. The notifications monthly-cap alert is refactored to consume this shared budget config rather than owning its own threshold.

**Framing note (inherited).** `cost_usd` is *API-equivalent* cost (token counts × price list), not necessarily real money — it's real for pay-as-you-go API users and notional for subscription users. Budget framing follows the same neutral language and the optional "I pay per-token (API billing)" copy flag established in the notifications brainstorm (R3 there). Budget computation is unaffected by that flag.

## Requirements

**Budget definition (the shared primitive)**
- R1. The user can set an optional **daily** budget and an optional **monthly** budget, each an independent USD amount with its own enable state. Either, both, or neither may be set. This is the canonical budget config consumed by the tray, the popover, and the notification rules.
- R2. Budgets are configured from a dedicated surface in the desktop app (the "Spend" section the notifications brainstorm introduces, so budgets and the alerts that consume them live together). Daily lives alongside monthly.
- R3. The **monthly** budget window is the local calendar month and resets at month rollover; the **daily** budget window is the local day `[midnight, next_midnight)` and resets at day rollover (reuses the existing day-boundary helper; monthly requires the new month-boundary helper already noted in the notifications brainstorm).
- R4. Spend measured against a budget counts **priced spend only** — unpriced rows (`cost_usd IS NULL`, unknown model) are excluded from the period SUM, not silently counted as $0. When unpriced rows exist in the period, the spend figure is a lower bound, and the popover budget view discloses the unpriced request count so a "healthy" color is never silently wrong.

**Menubar (tray title) display**
- R5. When at least one budget is set, the tray title shows the current spend followed by per-budget percent-used values, stacked beside the spend: `$123.45  D 75% / M 25%` (D and M on stacked lines next to the spend figure). Only budgets that are set appear (daily-only shows just `D`, etc.).
- R6. Each D/M percent value is tinted to its color band (see R9) in the menubar where the rendering layer allows it.
- R7. A user toggle controls whether the budget **percent figures** (D/M) appear in the tray title. When off, the tray shows the current spend without the D/M percentages. The warning indicator (R8) is independent of this toggle and still appears at amber or above, so turning off the figures never blinds the user to a budget warning.
- R8. When any set budget is at **amber or above** (≥76% used; see R9), a warning indicator (⚠) appears in the tray title and persists through the red band, regardless of the R7 toggle. The indicator reflects the worst current state across set budgets; the popover (R10/R11) shows which specific budget(s) are in that state.

**Color states & popover budget view**
- R9. Budgets have **four contiguous color states** by percent used (computed as priced spend ÷ budget, displayed rounded to a whole percent): green ≤50%, yellow >50–75%, amber >75–90%, red >90%. The bands are contiguous with no numeric gap at the edges, and the ⚠ threshold (R8) keys off the same rounded value as the tint so they never disagree. ≥100% is the *exceeded* state (see R11). These bands drive both the tinting (R6) and the popover progress bars.
- R10. The popover gains a budget section (placed under the "Today" header per the mockup) showing, for each set budget, a labeled progress bar — `DAILY BUDGET  $70 / $100` style — filled and colored to its band, acting as a countdown toward the budget.
- R11. A budget at amber or above shows a ⚠ on its row in the popover; the row's color and bar communicate the exact state. The view distinguishes "approaching" (amber/red under 100%) from "exceeded" (≥100%).
- R12. The budget section appears only when at least one budget is set; with no budgets set, the popover is unchanged from today.

**Notifications (consume the shared budget config)**
- R13. For each set budget — **both daily and monthly get approach and breach** — fire an **approach** notification exactly once per period when spend first crosses a configurable approach threshold (default **76%**, aligned to the amber band entry so the ambient color and the notification agree), and a **breach** notification exactly once per period when spend first crosses 100%; after either fires for a period it does not repeat on subsequent ingests. This reuses the notifications brainstorm's approach/breach machinery, now applied to both budgets rather than monthly only.
- R14. Notification dedup is per budget, per period, per threshold: each fires at most once per day (daily budget) or once per month (monthly budget), never repeatedly on subsequent ingests. All anti-spam, quiet-hours, residency, and unpriced-disclosure behavior specified in the notifications brainstorm applies unchanged (backfill handling is overridden by R17 below).
- R15. The monthly budget value (R1) is the **authoritative threshold**; the notifications feature reads it rather than holding its own cap value. The wiring change to the notifications brainstorm's monthly-cap rule (its R7–R11 → consume the shared budget) is owned by that doc's plan; a daily budget rule group is added mirroring the monthly one.
- R16. **Editing a budget mid-period** clears that budget's approach/breach dedup latches for the current period and triggers an immediate re-evaluation on save; any threshold now crossed — including a breach when the budget is **lowered below current spend** — fires once. Normal once-per-period dedup resumes thereafter.
- R17. **Backfill vs visual split.** The tray/popover visual state always reflects the true period total, including spend inserted by the startup backfill. Notifications fire only on **live forward-progress** crossings — backfilled past-dated rows never trigger approach/breach. On launch where a period is already over a threshold the budget fires a single catch-up notification, bounded once per period by a latch. *(Planning refinement: the catch-up applies to **both** daily and monthly breach, keyed by day/month respectively — superseding the original "daily stays visual-only" call, since the once-per-day latch prevents the per-launch storm that motivated the exclusion. See `docs/plans/2026-06-12-002-feat-budgets-plan.md`.)*

## Success Criteria
- With a daily and monthly budget set, the tray reads e.g. `$123.45  D 75% / M 25%` and the values update live as spend lands; per-band color tinting applies where the macOS rendering layer supports it (otherwise the values render uncolored).
- Crossing into amber (≥76%) on any set budget surfaces the ⚠ in the tray and on that budget's popover row; the indicator clears when spend (or the period reset) drops it below amber.
- The popover budget bars fill and recolor across the four bands and read as a clear countdown ("$70 / $100").
- Each budget produces exactly one approach notification and one breach notification per period, with no repeats and no launch-time storm from backfill (daily never catch-up-notifies on launch; monthly catch-up-notifies once if launched already over).
- Lowering a budget below current period spend fires one breach notification on save; the tray/popover go red immediately and reflect the true total.
- Turning the tray budget toggle off returns the tray to the plain spend figure with no budget text or ⚠.
- A budget never shows a healthy color while *priced* usage is over it: unpriced rows are excluded from the spend figure and their count is disclosed in the popover, so the displayed percent is an honest lower bound (it cannot account for the unknown cost of unpriced rows).
- There is exactly one place a monthly limit is configured; the notification cap-alert and the visualizations all read the same value.

## Scope Boundaries
- **No per-project budgets.** Budgets are account-wide (total spend), not per-cwd/per-project. Top-projects stays informational.
- **No weekly or custom-period budgets.** Daily and monthly only.
- **No hard enforcement.** Budgets warn and visualize; they never pause capture, block requests, or change Claude behavior. Farthing only observes.
- **No multi-currency.** USD only, matching `cost_usd`.
- **No change to cost computation.** Consumes existing `cost_usd`; pricing/backfill untouched.
- **Burst/delta alerts stay in the notifications brainstorm.** This doc owns budget definition + visualization + the budget-derived approach/breach alerts; the rolling-window burst and recurring-delta alerts remain notifications-brainstorm scope and are unaffected.

## Key Decisions
- **Budgets are the canonical config; notifications consume them** (R1, R15): a cap and a budget are the same number. Defining it once and refactoring the notifications monthly-cap to read from it removes the two-configs-to-keep-in-sync failure mode and makes the daily budget a near-free addition to the existing approach/breach machinery.
- **Daily budget added** (R1, R3): the notifications brainstorm explicitly excluded a daily cap; the user now wants daily visibility. The day-boundary helper already exists, so daily is the cheap half — monthly carries the only new boundary work.
- **Four color bands, percent in the tray** (R5, R9): percent pairs directly with the bands and is scale-independent; tinting the tray values to their band gives an at-a-glance state without reading the number. Four bands (vs two/three) was the user's explicit calibration.
- **Tray ⚠ at amber+ (≥76%), not at exceed** (R8): the warning earns its place by giving lead time before the breach, and persists through red so it never flickers off near the ceiling.
- **Approach + breach notifications for both budgets, approach default aligned to amber at 76%** (R13): visual amber/red is ambient; the notification is the active interruption. Aligning the approach default to the amber band entry (76%) means the tray color and the notification agree out of the box — one threshold to reason about, still tunable. Daily gets the full pair (not just breach) at the user's call, accepting that heavy days may ping; the dedup keeps it to one approach + one breach per day.
- **Mid-period edits reset the dedup latch and re-evaluate** (R16): a budget's denominator is mutable but the once-per-period latch is not tied to it; clearing the latch on edit and re-firing the now-crossed threshold closes the "lowered my budget below what I've already spent and heard nothing" gap that fire-on-raise alone misses.
- **Visual reflects true total; notifications fire on live progress only; daily skips launch catch-up** (R17): the startup backfill inserts past-dated rows. Letting them drive the bar/⚠ keeps the visual honest, but letting them drive notifications would storm — especially daily, which you routinely launch already part-spent. Monthly gets a single launch catch-up (a real ceiling worth one ping); daily relies on its visual at launch.
- **Priced-spend-only, disclose unpriced** (R4): inherited from the cap decision — coalescing NULL cost to $0 would let a budget under-report and show green while genuinely over.
- **Warn, never enforce** (Scope): Farthing is an observer; a budget that paused capture or throttled work would be a different, riskier product.

## Dependencies / Assumptions
- **Coupled to the notifications brainstorm.** R13–R15 assume the notifications feature's notification plumbing (tauri-plugin-notification wiring, approach/breach dedup, quiet hours, residency warning, backfill suppression) — verified absent today and specced there. Budgets should ship with or after that plumbing; the visualization (R5–R12) can land independently of notifications.
- **Month-boundary helper** (R3): the existing boundary logic in `src-tauri/src/metrics.rs` is day-granular; monthly budgets need the first-of-month helper the notifications brainstorm already flagged. Shared between both features.
- **Tray title is single-string today** (`src-tauri/src/tray_title.rs`, `format_title`). The stacked two-line layout (R5) and per-value color tinting (R6) depend on what the macOS status-item rendering supports (attributed strings / multi-line); feasibility is a planning question, with single-line `$123.45 | D 75% M 25%` as the fallback.
- **Popover refresh path exists** (`src/routes/popover/+page.svelte` refetches on `ingest:stored`); the budget section consumes the same metrics refresh, plus monthly-to-date spend which `today_metrics` does not currently return.
- `cost_usd` is API-equivalent; budget framing inherits the optional API-billing copy flag (R3 of the notifications brainstorm).

## Outstanding Questions

### Resolve Before Planning
*(none — product behavior is settled by the decisions above)*

### Deferred to Planning
- [Affects R5/R6][Technical][Needs research] Whether the macOS tray status item can render two stacked lines and per-substring color via attributed strings, or whether single-line uncolored is the practical ceiling. Determines the R5/R6 fallback.
- [Affects R3][Technical] Month-boundary helper implementation (first-of-month `NaiveDate` + local-midnight ms) with DST/clock-change tests — shared with the notifications brainstorm; build once.
- [Affects R8/R9][Technical] Where the "worst state across set budgets" and percent-used values are computed (Rust tray-refresh path vs frontend) and how the tray refresh reads monthly-to-date spend, which today's tray query (today-only SUM) doesn't provide.
- [Affects R10][Design] Exact popover budget-section layout: placement relative to the token split / sparkline, bar styling, and the amber/red/exceeded row treatment (the mockup is the reference).
- [Affects R7][Design] Where the "show budgets in tray" toggle lives (Spend section vs Settings) and whether it defaults on or off when a budget is first set.
- [Affects R11][Design] Visual treatment of the *exceeded* (≥100%) state — overflow fill, cap-and-flag, or a distinct label — distinct from the red band under 100%.
- [Affects R1/R2][Design] Budget input UX in the Spend section: input control, validation, a minimum sensible value (a near-zero budget renders meaningless percent), save model, and empty state. Likely shares the notifications brainstorm's Spend-section design.
- [Affects R3/R10][Design] Period-rollover refresh: at day/month boundary the bar and ⚠ reset with no ingest event; confirm the existing 60s tick (notifications brainstorm) drives the popover/tray refresh so the reset is visible.
- [Affects R9][Design] Dark-mode color values for the four bands (and bar-label contrast), following the existing popover dark-mode pattern (`prefers-color-scheme: dark` block, reduced-opacity system colors) rather than literal light-mode system green/amber/red.

## Next Steps
-> `/ce:plan` for structured implementation planning (sequence behind or alongside the cost-notifications notification plumbing, since R13–R15 depend on it)
