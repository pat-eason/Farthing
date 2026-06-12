# Cost alerts (notifications plumbing + burst/delta)

How the cost-notification feature persists its state and why it discriminates
live spend from recovered history by *event time*, not by `source`. See the
plan at `docs/plans/2026-06-12-001-feat-cost-notifications-plan.md`.

## The `meta` keys

`AlertState` (`src-tauri/src/alerts.rs`) mirrors `CaptureState`: a DB handle plus
an in-memory cache, persisted as JSON in the `meta` table. Where `CaptureState`
holds a single `AtomicBool`, the alert feature carries a composite blob, so it
stores two keys. Both load resiliently (absent/malformed JSON → documented
defaults, no panic) and every field carries a serde default, so the Budgets plan
can add fields without a migration.

### `alert_config` — what the user tuned in the Spend UI

```json
{
  "delta": { "enabled": false, "step_usd": 50.0, "quiet": null },
  "burst": {
    "enabled": true,
    "threshold_usd": 10.0,
    "window_minutes": 10,
    "cooldown_minutes": 15,
    "quiet": null
  },
  "api_billing": false
}
```

- **delta** — the recurring milestone rule ("every $N of spend"). Disabled by
  default; `step_usd` defaults to $50.
- **burst** — the session/runaway-loop rate rule ("$N in a rolling window").
  Enabled by default at $10 / 10 min window / 15 min cooldown: a runaway agent
  loop is the motivating day-one danger, and the 10-minute window keeps it from
  firing on a legitimately heavy session.
- **quiet** (per rule) — `null` means always allowed; otherwise
  `{ "start": "22:00", "end": "07:00" }`, local `"HH:MM"` 24-hour strings the
  engine compares wrap-aware against `Local::now()`. A wrap window (`start > end`)
  is overnight; `start == end` is treated as unset.
- **api_billing** — the "I pay per-token" flag; switches notification copy from
  neutral usage wording to real-money wording. Off by default (cost is notional
  for subscribers).

The shape leaves room for the Budgets plan's budget-derived approach/breach
config; this plan ships only delta + burst.

### `alert_runtime` — edge-trigger / cooldown bookkeeping

```json
{
  "delta": { "month_key": "2026-06", "last_step": 2 },
  "burst": { "cooldown_until_ms": 1781150400000 },
  "permission_lost": false
}
```

- **delta.month_key** — the calendar month (`"%Y-%m"`, local) the baseline tracks;
  a month rollover re-baselines so steps never carry across months.
- **delta.last_step** — the highest milestone index already fired this month
  (`floor(MTD / step_usd)` at the last fire). Backfill re-baselines this silently.
- **burst.cooldown_until_ms** — unix ms before which burst won't fire again; `0`
  means unarmed. Stored as **UTC ms** so a DST fall-back's repeated local hour
  can't reopen the cooldown early.
- **permission_lost** — set when a `show` returned `PermissionDenied` (user
  revoked or never granted notification permission); surfaced in the Spend UI so
  silent non-coverage becomes visible.

Two pieces of state are deliberately *not* persisted: `process_start_ms` (see
below; meaningful only for the current process) and the eval lock (`Mutex<()>`
held across the gather→evaluate→persist cycle so concurrent ingest-path, tick,
and config-save evaluations can't interleave into a lost update).

## Live vs historical: the `process_start_ms` floor (why not `source`)

Burst and delta must count only spend that happened *while the app was running*.
Recovered history must never trip a live alert: a backfill pass that imports a
runaway session from when the app was off cannot be allowed to flood a burst or
replay milestones.

The obvious discriminator — "only count rows with `source = 'otel'`" — is
**defeated by the otel-wins upsert** in `src-tauri/src/ingest.rs`. When a
`source = 'backfill'` row collides on `request_id` with a re-delivered OTLP
export, the upsert flips it (`ON CONFLICT … DO UPDATE SET … source = 'otel'`,
`WHERE requests.source = 'backfill'`) — and in the same statement sets
`timestamp_ms = excluded.timestamp_ms`, i.e. the **real event time of the API
request**. That event time is when the request actually happened, which for a
recovered session is *before* this launch. So a row that lived through the app
being off can end up flagged `source = 'otel'`: the source flag is not a reliable
live-vs-historical signal.

The robust discriminator is the **event time** itself. At startup `AlertState`
captures `process_start_ms` (wall-clock unix ms) in memory, and the burst/delta
spend queries floor on it:

- burst sums rows over `[max(now - window, process_start_ms), now)`;
- delta MTD counts rows timestamped `>= process_start_ms`.

A recovered pre-launch row is dated before boot, so it is excluded **regardless
of any `source` flip or row id**. The gate lives in the query assembly
(`gather_sums`), not inside the pure `evaluate`, so the discrimination — the
highest-risk correctness surface — carries its own tests. In normal operation
(app up longer than the window) the rolling-window start is the binding
constraint and the floor is inert; it only bites right after launch, exactly when
backfill and otel re-delivery are recovering history.

Delta layers a second guard on top: each backfill-pass completion silently
re-baselines `last_step` to the current post-launch MTD step (no fire), so only
genuine post-launch growth advances the ladder.
