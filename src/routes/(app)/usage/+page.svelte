<script lang="ts">
  // Plan usage view: displays all four subscription usage windows (5h session,
  // 7d overall, 7d Sonnet, 7d Opus) plus the optional extra-usage credits
  // section. Data comes from the backend usage-limits poller that runs every
  // 5 minutes; the view subscribes to live updates via the usage:updated event
  // so it refreshes without user interaction. A manual Refresh button is
  // provided for immediate re-polls.
  //
  // States handled:
  //   - null snapshot (no fetch yet): "Loading" message
  //   - status === 'unauthenticated': sign-in prompt
  //   - status === 'unavailable': stale-data warning banner + last known data
  //   - config.enabled === false: banner directing user to Settings
  //   - snapshot older than 10 minutes: per-card "as of Xm ago" annotation
  import { invoke } from "@tauri-apps/api/core";
  import {
    getUsageStatus,
    getUsageLimitsConfig,
    onUsageUpdated,
    formatResetIn,
    type UsageSnapshot,
    type UsageLimitsConfig,
    type WindowSnapshot,
  } from "$lib/usage";

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------

  let snapshot: UsageSnapshot | null = $state(null);
  let config: UsageLimitsConfig | null = $state(null);
  let loadError = $state("");
  let refreshing = $state(false);
  let initialized = $state(false);

  // ---------------------------------------------------------------------------
  // Derived helpers
  // ---------------------------------------------------------------------------

  /** True when the snapshot is more than 10 minutes old. */
  const isStale = $derived(
    snapshot !== null && Date.now() - (snapshot as UsageSnapshot).fetched_at_ms > 10 * 60 * 1000
  );

  /** How many minutes ago the snapshot was fetched (for the stale annotation). */
  const fetchedMinsAgo = $derived(
    snapshot ? Math.floor((Date.now() - (snapshot as UsageSnapshot).fetched_at_ms) / 60_000) : 0
  );

  // ---------------------------------------------------------------------------
  // Data loading + event subscription
  // ---------------------------------------------------------------------------

  async function load() {
    loadError = "";
    try {
      const [snap, cfg] = await Promise.all([getUsageStatus(), getUsageLimitsConfig()]);
      snapshot = snap;
      config = cfg;
    } catch (err) {
      loadError = String(err);
    } finally {
      initialized = true;
    }
  }

  async function refresh() {
    refreshing = true;
    try {
      // Re-invoke the status command to trigger an immediate backend poll.
      // The backend updates its cache and the updated snapshot comes back here.
      snapshot = await invoke<UsageSnapshot | null>("usage_limits_status");
    } catch (err) {
      loadError = String(err);
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    void load();

    // Subscribe to live updates from the 5-minute poller.
    const unlistenPromise = onUsageUpdated((snap) => {
      snapshot = snap;
    });

    return () => {
      void unlistenPromise.then((stop) => stop());
    };
  });

  // ---------------------------------------------------------------------------
  // Progress bar helpers
  // ---------------------------------------------------------------------------

  /**
   * Map a usage percent to a CSS color class.
   * < 75 → normal (green), 75–90 → warning (yellow), > 90 → danger (red).
   */
  function barColorClass(percent: number | null): string {
    if (percent === null) return "bar-unknown";
    if (percent > 90) return "bar-danger";
    if (percent >= 75) return "bar-warning";
    return "bar-ok";
  }

  function pctLabel(percent: number | null): string {
    if (percent === null) return "—";
    return `${Math.round(percent)}% used`;
  }

  function creditsLabel(used: number | null, limit: number | null): string {
    if (used === null || limit === null) return "—";
    return `${used.toFixed(2)} / ${limit.toFixed(2)} credits`;
  }

  const windows = $derived(
    snapshot
      ? [
          (snapshot as UsageSnapshot).five_hour,
          (snapshot as UsageSnapshot).seven_day,
          (snapshot as UsageSnapshot).seven_day_sonnet,
          (snapshot as UsageSnapshot).seven_day_opus,
        ]
      : ([] as WindowSnapshot[])
  );
</script>

<main class="container">
  <div class="page-header">
    <h1>Plan Usage</h1>
    <p class="subtitle">Updates every 5 minutes · Subscription plan only</p>
  </div>

  {#if loadError}
    <section class="warn-box error-box">
      <p>{loadError}</p>
    </section>
  {/if}

  {#if !initialized}
    <p class="muted">Loading…</p>
  {:else if config && !config.enabled}
    <!-- Poller disabled: direct user to settings -->
    <section class="warn-box">
      <p>
        Plan usage polling is disabled. Enable it in
        <strong>Settings &rarr; Plan Usage</strong> to start seeing subscription limits here.
      </p>
    </section>
  {:else if !snapshot || snapshot.status === "unauthenticated"}
    <!-- No data or session expired -->
    <section class="card auth-prompt">
      <p>Sign in to Claude Code to view plan usage.</p>
      <p class="muted">
        Once signed in, enable plan usage tracking in Settings and data will appear here.
      </p>
    </section>
  {:else}
    <!-- We have data. Show the unavailability banner if needed, then the cards. -->
    {#if snapshot.status === "unavailable"}
      <section class="warn-box">
        <p>Unable to reach Anthropic. Showing last known values.</p>
      </section>
    {/if}

    {#if isStale}
      <p class="stale-note muted">Showing data from {fetchedMinsAgo}m ago.</p>
    {/if}

    <!-- Four usage-window cards -->
    <div class="windows-grid">
      {#each windows as win (win.label)}
        <section class="card window-card">
          <h2 class="window-label">{win.label}</h2>

          <!-- Progress bar -->
          <div
            class="bar-track"
            role="progressbar"
            aria-valuenow={win.percent ?? 0}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div
              class="bar-fill {barColorClass(win.percent)}"
              style="width: {Math.min(win.percent ?? 0, 100)}%"
            ></div>
          </div>

          <div class="window-meta">
            <span class="pct-label">{pctLabel(win.percent)}</span>
            <span class="reset-label muted">{formatResetIn(win.resets_at_ms) || "—"}</span>
          </div>
        </section>
      {/each}
    </div>

    <!-- Extra usage credits (add-on feature, shown only when enabled) -->
    {#if snapshot.extra_usage?.is_enabled}
      {@const extra = snapshot.extra_usage}
      <section class="card extra-section">
        <h2>Extra usage</h2>
        <p class="muted">Add-on credits purchased beyond your plan allowance.</p>

        {#if extra.monthly_limit !== null}
          <!-- Show a bar for extra usage too -->
          <div
            class="bar-track extra-bar"
            role="progressbar"
            aria-valuenow={Math.round((extra.utilization ?? 0) * 100)}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div
              class="bar-fill {barColorClass(
                extra.utilization !== null ? extra.utilization * 100 : null
              )}"
              style="width: {Math.min((extra.utilization ?? 0) * 100, 100)}%"
            ></div>
          </div>
        {/if}

        <p class="credits-label">{creditsLabel(extra.used_credits, extra.monthly_limit)}</p>
      </section>
    {/if}

    <!-- Refresh button -->
    <div class="actions">
      <button disabled={refreshing} onclick={() => void refresh()}>
        {refreshing ? "Refreshing…" : "Refresh"}
      </button>
    </div>
  {/if}
</main>

<style>
  .container {
    max-width: 720px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
    text-align: left;
  }

  .page-header {
    margin-bottom: 1.25rem;
  }

  h1 {
    font-size: 1.4rem;
    margin: 0 0 0.2rem;
  }

  .subtitle {
    font-size: 0.8rem;
    color: #6b6b6b;
    margin: 0;
  }

  h2 {
    font-size: 1.05rem;
    margin: 0 0 0.5rem;
  }

  .muted {
    color: #6b6b6b;
    margin: 0.25rem 0;
  }

  /* Warning / error banners */
  .warn-box {
    border: 1px solid #d4a72c;
    background-color: rgba(212, 167, 44, 0.08);
    border-radius: 10px;
    padding: 0.85rem 1.1rem;
    margin-bottom: 1rem;
  }

  .warn-box p {
    margin: 0;
  }

  .error-box {
    border-color: #b42318;
    background-color: rgba(180, 35, 24, 0.06);
  }

  /* Stale age annotation */
  .stale-note {
    font-size: 0.82em;
    margin-bottom: 0.75rem;
  }

  /* 2-column grid for the four window cards */
  .windows-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.9rem;
    margin-bottom: 1rem;
  }

  /* Cards */
  .card {
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 10px;
    padding: 1rem 1.25rem;
    background: #ffffff;
  }

  .window-card {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .window-label {
    font-size: 0.9rem;
    font-weight: 600;
    margin: 0;
    color: #3a3a3c;
  }

  /* Progress bar */
  .bar-track {
    height: 8px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.08);
    overflow: hidden;
  }

  .bar-fill {
    height: 100%;
    border-radius: 4px;
    transition: width 0.2s ease;
  }

  .bar-ok {
    background: #1a7f37;
  }

  .bar-warning {
    background: #d4a72c;
  }

  .bar-danger {
    background: #b42318;
  }

  .bar-unknown {
    background: rgba(0, 0, 0, 0.2);
  }

  /* Window meta row below the bar */
  .window-meta {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    font-size: 0.82rem;
  }

  .pct-label {
    font-weight: 500;
  }

  .reset-label {
    font-size: 0.78rem;
  }

  /* Auth prompt card */
  .auth-prompt {
    text-align: center;
    padding: 2rem;
  }

  .auth-prompt p {
    margin: 0.3rem 0;
  }

  /* Extra usage section */
  .extra-section {
    margin-bottom: 1rem;
  }

  .extra-section h2 {
    margin-bottom: 0.35rem;
  }

  .extra-bar {
    margin: 0.65rem 0 0.4rem;
  }

  .credits-label {
    font-size: 0.88rem;
    font-weight: 500;
    margin: 0;
  }

  /* Refresh button row */
  .actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 0.5rem;
  }

  button {
    border-radius: 8px;
    border: 1px solid rgba(0, 0, 0, 0.15);
    padding: 0.55em 1.1em;
    font-size: 0.95em;
    font-weight: 500;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
    cursor: pointer;
  }

  button:hover:not(:disabled) {
    border-color: #396cd8;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  /* Dark mode */
  @media (prefers-color-scheme: dark) {
    .subtitle {
      color: #a8a8a8;
    }

    .muted {
      color: #a8a8a8;
    }

    .warn-box {
      border-color: #d4a72c;
      background-color: rgba(212, 167, 44, 0.12);
    }

    .error-box {
      border-color: #ffa198;
      background-color: rgba(255, 161, 152, 0.1);
    }

    .card {
      background: #1f1f21;
      border-color: rgba(255, 255, 255, 0.15);
    }

    .window-label {
      color: #e5e5e7;
    }

    .bar-track {
      background: rgba(255, 255, 255, 0.12);
    }

    .bar-ok {
      background: #7ee787;
    }

    .bar-warning {
      background: #d4a72c;
    }

    .bar-danger {
      background: #ffa198;
    }

    .bar-unknown {
      background: rgba(255, 255, 255, 0.2);
    }

    button {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }
  }
</style>
