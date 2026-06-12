---
date: 2026-06-12
topic: cost-notifications
---

# Cost Notifications (Spend Cap & Usage Alerts)

## Problem Frame

Farthing already shows today's live cost in the tray title, refreshed on every ingest plus a 60s tick. A glanceable total is good for *awareness* but useless for *protection*: it can't warn you that you're about to blow a monthly spend cap, and you won't notice a runaway agent loop burning cost while your eyes are on the code.

The goal is **spend-cap and usage alerts**: native desktop notifications that fire when spend crosses a ceiling, climbs by a milestone, or spikes unusually fast. Each notification must earn its interruption — it should carry information a glance at the tray cannot (a crossed cap, a fast burn). Anything that just restates the visible number is notification fatigue and will get muted.

**A framing note that shapes the copy.** Farthing's `cost_usd` is *API-equivalent* cost, computed from token counts and a price list; the app has no signal for how a user actually pays. For pay-as-you-go API users it's real money; for subscription (Max/Pro) users it's a notional utilization metric. So the default language is a neutral "spend cap" / "usage alert," and an optional "I pay per-token (API billing)" setting (default off) switches the copy to real-money budget-guard framing for the users it actually applies to. Cost computation does not change either way.

## Requirements

**Framing and management surface**
- R1. A dedicated "Spend" section in the desktop app, separate from Settings, where all alert rules are viewed, enabled, and configured.
- R2. Each alert type is an independently configurable rule with its own enable toggle, type-specific threshold value(s), and its own quiet-hours window. The data model is a list of per-type rules, not one global config blob.
- R3. Default alert copy frames notifications as a "spend cap" / "usage alert" on API-equivalent cost. An optional "I pay per-token (API billing)" setting (default off) switches alert copy and section labels to real-money budget framing. This affects wording only, not cost computation or thresholds.
- R4. A "send test notification" action (per rule) verifies OS notification permission is granted and previews that rule's copy. On denied permission, it shows an inline recovery affordance that deep-links to macOS notification settings.
- R5. Notification permission is requested lazily — on first rule enable or first test action, whichever comes first. The Spend section surfaces permission state (never-requested / granted / denied) and the denied-recovery path, so a silently no-op'd notification is never a mystery.
- R6. (Optional, low cost) A short "recently fired" list in the Spend section so the user can see what triggered and when. If included, planning specifies its content and empty state; if cut, the underlying last-fired state still exists for debugging.

**Monthly cap alert**

> **Superseded by the Budgets brainstorm** (`docs/brainstorms/2026-06-12-budgets-requirements.md`). The "monthly cap" below is now the **monthly budget** defined there; budgets are the canonical config (daily + monthly) and these alert rules consume them. A parallel daily-budget alert group mirrors R8/R9 for the daily window. R7's cap value is read from the shared budget rather than configured here. The approach/breach/dedup/quiet-hours/backfill behavior in R8–R11 and R16–R21 is unchanged and applies per budget.

- R7. User sets a monthly USD cap (now the monthly budget; see Budgets brainstorm R1). The window is the local calendar month and resets at month rollover. (Requires a new month-boundary helper; the existing DST-correct boundary logic is day-granular only.)
- R8. An *approach* warning fires once per calendar month when month-to-date spend crosses a configurable percentage of the cap (default 80%).
- R9. A *breach* alert fires once per calendar month when month-to-date spend crosses 100% of the cap.
- R10. Cap evaluation counts priced spend only — unpriced rows (`cost_usd IS NULL`, unknown-model) are excluded from the month-to-date SUM rather than silently counted as $0. When unpriced rows exist in the period, month-to-date spend is a lower bound, and the alert plus the Spend view disclose the unpriced request count so a "green" state is never silently wrong.
- R11. When a user first sets or raises a cap and month-to-date spend already exceeds a threshold, fire that threshold's alert once immediately on save; normal once-per-month dedup applies thereafter. (Prevents being blind for the rest of the first month.)

**Recurring delta alert**
- R12. Fire each time cumulative spend increases by a configurable $N step (default $50) — ambient awareness milestones.
- R13. The delta counter resets at the start of each calendar month, staying aligned with the monthly-cap window.

**Session / burst alert**
- R14. Fire when spend within a rolling time window exceeds a configurable threshold (default: $N in the last 10 minutes), to catch a runaway agent loop quickly. This is a rolling-window *rate*, not a per-session total — a long, cheap session must not trip it, and a fast burn must trip it regardless of session boundaries.
- R15. After firing, a cooldown suppresses repeat burst alerts for a configurable interval (default 15 minutes) so one runaway loop produces one alert, not a stream.

**Timing, delivery, quiet hours, anti-spam**
- R16. Burst (R14) and delta (R12) evaluate on live forward progress only: the rolling window is computed over event time (`timestamp_ms`), and the startup backfill pass — which inserts past-dated rows — must not retroactively trigger burst or delta (no alert storm on launch). The monthly cap alerts (R8/R9) are month-to-date-total-based and fire once per threshold per month regardless of whether the spend arrived live or via backfill.
- R17. Each alert type has its own quiet-hours window (start/end local time). During a type's quiet window, that type is suppressed independently of the others.
- R18. Alerts are delivered as native OS notifications. Clicking a cap alert (approach/breach) opens the Spend section; clicking a delta or burst alert opens the existing Cost view (`/(app)/cost`).
- R19. Every alert is edge-triggered and de-duplicated: each distinct condition fires once per occurrence, never repeatedly on every ingest (monthly approach/breach once per month; delta once per $N step; burst gated by its cooldown).
- R20. Quiet-hours catch-up: when a *monthly breach* (R9) is suppressed by quiet hours and the account is still over cap on quiet-hours exit, fire a single catch-up notification. All other alert types drop silently when suppressed by their quiet hours rather than queueing.
- R21. Residency precondition: alerts only fire while Farthing is running. When any rule is enabled but "start at login" is off, the Spend section surfaces a "alerts only run while Farthing is open" warning with a one-click fix, so silent non-coverage is visible rather than assumed.

## Alert Type Summary

| Alert | Triggers on | Default | Urgency | Re-fire rule |
|---|---|---|---|---|
| Monthly cap — approach | MTD priced spend crosses % of cap | 80% | Medium | Once per month |
| Monthly cap — breach | MTD priced spend crosses 100% of cap | cap = user-set | High | Once per month (+ catch-up on quiet-hours exit) |
| Recurring delta | Cumulative spend +$N (live) | $50 | Low | Once per $N step, resets monthly |
| Session / burst | $N in last M min (event-time) | $N in 10 min | High (now) | Cooldown (15 min) |

*Forecast ("on track to exceed cap") is deferred to v2 — see Scope Boundaries.*

## Success Criteria

- A runaway agent loop (sharp spend spike in live ingest) produces a burst alert within roughly a minute of the spike.
- Crossing 80% and 100% of the monthly cap each produces exactly one notification, with no repeats on subsequent ingests.
- The cap never shows a "green" state while real usage is over it: unpriced rows are excluded from the total and their count is disclosed, so the user knows MTD is a lower bound.
- Launching the app (which runs a backfill of past-dated rows) never produces a burst/delta alert storm.
- Setting or raising a cap mid-month while already over a threshold produces one immediate alert, not silence for the rest of the month.
- No notification fires that merely restates the already-visible tray total without new information.
- Every rule (threshold, quiet hours, enable/disable) is configurable and testable from the Spend section without restarting the app.

## Scope Boundaries

- **Forecast deferred to v2.** A "projected month-end exceeds cap" alert is a fast-follow once the projection formula and its early-month jumpiness are resolved (see Deferred to v2). v1 ships the four alert types in the summary table.
- **No plan-type detection.** Farthing has no signal for subscription vs API billing; the optional "I pay per-token" copy flag (R3) replaces any attempt to auto-detect.
- **No hourly periodic digest.** The original "notify me every hour" idea is dropped: redundant with the always-visible tray cost and superseded by event-driven alerts. (A low-frequency end-of-day summary could revisit this later; hourly is out.)
- ~~**No daily cap.**~~ Superseded: the Budgets brainstorm adds a daily budget with its own approach/breach alert group mirroring the monthly one.
- **Native OS notifications only.** No email, Slack, or push-to-phone delivery.
- **USD only.** Matches the existing `cost_usd`; no multi-currency.
- **No change to cost computation.** Consumes existing `cost_usd`; pricing/backfill logic is untouched.

## Key Decisions

- **Neutral "spend" framing + optional API-billing flag** (R3): `cost_usd` is notional for subscription users, so real-money "budget guard" language can't be the default. The flag lets the feature be a genuine budget guard for API users without misleading everyone else, at the cost of one boolean and a copy switch.
- **Defer forecast to v2** (Scope): once the cap query and notification plumbing exist, approach/breach/delta are nearly free (same query, different thresholds). Forecast is the one piece with unresolved logic (projection formula) and a real failure mode (early-month jumpiness firing false warnings), so it ships separately.
- **Burst/delta on live forward progress; cap on MTD totals** (R16): the startup backfill inserts past-dated rows, which would otherwise either spuriously trip every threshold at launch (ingest-time window) or be invisible to burst (event-time window). Splitting the semantics — event-time live evaluation for burst/delta, total-based once-per-month for the cap — resolves the ambiguity coherently.
- **Exclude unpriced rows and disclose the count** (R10): the existing totals query coalesces NULL cost to $0, which would let the cap under-report and never fire. Excluding-and-disclosing keeps the guard honest.
- **Immediate fire on mid-month cap-set over threshold** (R11): "fires once when it crosses" would silently miss a breach the user is already past when they set the cap.
- **Residency surfaced, not assumed** (R21): a guard that's silent when the app is quit is the worst kind of false safety; the most common failure (autostart off) gets a visible warning.
- **Per-alert-type quiet hours** (R17): kept for flexibility on a personal tool, accepting the extra config surface; a single global window remains a possible simplification if the surface proves annoying.
- **Edge-triggered + cooldown everywhere** (R19): the entire feature's value depends on not becoming noise; dedup is a first-class requirement.

## Dependencies / Assumptions

- **Tauri notification plugin is not currently a dependency** (verified absent in `src-tauri/Cargo.toml`). Enabling it requires three coordinated changes, not just the crate: (1) `tauri-plugin-notification` in `src-tauri/Cargo.toml`, (2) `.plugin(tauri_plugin_notification::init())` in the `lib.rs` builder chain, and (3) a `notification:default` permission entry in `src-tauri/capabilities/default.json` (which currently grants only `core:default` and `opener:default`).
- Alert evaluation hooks into the existing ingest pipeline (immediate path, for burst) and the existing 60s tick (month rollover, quiet-hours-exit catch-up). Verified both exist (`ingest.rs` notifier, 60s `tokio::time::interval` in `lib.rs`). Note: the ingest notifier currently passes only a stored-row count, not spend — the alert path will need to query the DB itself (see Deferred to Planning).
- Alert rule config and last-fired bookkeeping persist in the database; the `meta` key-value table is the current pattern, but the per-type-rule model in R2 likely fits a dedicated table better (planning decision).
- `cost_usd` is API-equivalent (token counts × price list); for subscription users it is notional. This is the basis for R3's framing.

## Deferred to v2 / Fast-follow

- **Forecast alert**: fires when projected month-end spend is on track to exceed the cap, at most once per day. Blocked on resolving the projection formula (linear MTD extrapolation vs trailing-window run rate) and guarding against early-month jumpiness (e.g. a minimum-elapsed-days floor before projecting). Folds into the monthly-cap rule group when shipped.

## Outstanding Questions

### Resolve Before Planning
*(none — product behavior is settled by the decisions above)*

### Deferred to Planning
- [Affects R7][Technical] Month-boundary helper implementation (first-of-month `NaiveDate` + `local_midnight_ms`), with first-of-month and DST/clock-change tests.
- [Affects R14/R16][Technical] Rolling-window burst query shape over the `requests` table and an evaluation-frequency bound so a runaway loop (many exports/sec) doesn't trigger a DB query per export.
- [Affects R16][Technical] How alert evaluation reads spend: widen the `IngestNotifier` payload, add a second DB-owning callback, or query directly from the notifier closure (decides lock contention against the ingest write on the same connection).
- [Affects R2][Technical] Persist alert state in the `meta` KV table vs a dedicated rules/alert-state table; handle concurrent evaluation from both the ingest path and the 60s tick (fire/dedup races).
- [Affects R4/R5/R18][Technical] macOS notification permission API specifics, and whether tauri-plugin-notification's click callback can reliably re-enter the app and run the Accessory→Regular policy flip to open a view.
- [Affects R1/R2][Design] Spend-view layout (group the cap's sub-alerts under one cap-value card vs flat rows), the per-rule save model (auto-save vs Save button) and validation bounds, the quiet-hours time-input control, and sidebar placement.
- [Affects R8/R12/R14][Technical] Exact default values (approach %, delta $, burst $/window, cooldown) — propose sensible defaults; all user-tunable. For a near-zero-config tool the defaults effectively define the feature for most users.

## Next Steps
-> `/ce:plan` for structured implementation planning
