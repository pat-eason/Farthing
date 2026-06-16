<script lang="ts">
  // Popover content (tasks 4.2/4.3): today's cost (API-equivalent), token
  // split, session count, 7/30-day cost sparkline, and top 3 projects by
  // cost. The window is created hidden at startup and toggled by the tray
  // icon (task 4.1); data refreshes on focus (every open) and on the
  // backend's ingest push event while the page lives (task 4.4), so values
  // update live without reopening. A paused badge + resume button reflect
  // the capture pause state.
  import { tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { LogicalSize } from "@tauri-apps/api/dpi";
  import { getDailyCosts, getTodayMetrics, type DailyCost, type TodayMetrics } from "$lib/metrics";
  import {
    getCaptureStatus,
    setCapturePaused,
    INGESTED_EVENT,
    PAUSED_CHANGED_EVENT,
    type CaptureStatus,
  } from "$lib/capture";
  import {
    getBudgetStatus,
    BUDGET_CONFIG_CHANGED,
    METRICS_TICK_EVENT,
    type Band,
    type BudgetStatus,
  } from "$lib/budgets";
  import { openMainWindow } from "$lib/window";
  import {
    getUsageLimitsConfig,
    getUsageStatus,
    onUsageUpdated,
    onDisplayModeChanged,
    formatResetIn,
    type UsageSnapshot,
  } from "$lib/usage";
  import { formatCost, formatTokens, projectName } from "$lib/format";
  // Same 128px downscale the sidebar uses; rendered at 18px here.
  import farthingIcon from "$lib/assets/farthing-icon.png";
  import Sparkline from "$lib/Sparkline.svelte";

  /** Trailing debounce for ingest-push refreshes: one batched export can
   * store many rows but should trigger a single refetch. */
  const INGEST_REFRESH_DEBOUNCE_MS = 200;
  const SPARKLINE_RANGES = [7, 30] as const;

  /** Fixed popover width (matches tauri.conf.json). */
  const POPOVER_WIDTH = 320;
  /** Floor so a transient empty render can't collapse the window. */
  const MIN_POPOVER_HEIGHT = 120;

  let metrics: TodayMetrics | undefined = $state();
  let dailyCosts: DailyCost[] | undefined = $state();
  let budgetStatus: BudgetStatus | undefined = $state();
  /** The content element, measured to size the window to its content. */
  let popoverEl: HTMLElement | undefined = $state();
  let sparklineDays: (typeof SPARKLINE_RANGES)[number] = $state(7);
  let paused = $state(false);
  let errorMessage = $state("");
  /** Fetch + DOM update time of the last refresh (dev render budget check). */
  let renderMs = $state(0);
  let usageSnapshot: UsageSnapshot | null = $state(null);
  let usageEnabled = $state(false);

  async function refresh(days: number = sparklineDays) {
    const started = performance.now();
    // Budget status has its own try/catch so a budget query error never
    // blanks the popover; on failure the section just hides.
    try {
      budgetStatus = await getBudgetStatus();
    } catch {
      budgetStatus = undefined;
    }
    try {
      [metrics, dailyCosts] = await Promise.all([getTodayMetrics(), getDailyCosts(days)]);
      errorMessage = "";
      await tick();
      renderMs = performance.now() - started;
    } catch (err) {
      errorMessage = String(err);
    }
    await tick();
    void resizeToContent();
  }

  // Size the popover window to its content exactly (width stays 320). The
  // window is non-resizable (tauri.conf.json), but on macOS setSize maps to
  // NSWindow setContentSize:, which programmatic sizing honors regardless of
  // the resizable style mask — so no min/max pinning is needed to both fit the
  // content and keep the user from dragging the edges. Driven by a
  // ResizeObserver (below) so late layout — the budget section appearing,
  // fonts settling — always resizes after the fact. `.popover` is height:auto,
  // so this measures the true content height (not the current window height).
  async function resizeToContent() {
    if (!popoverEl) return;
    const h = Math.max(MIN_POPOVER_HEIGHT, Math.ceil(popoverEl.getBoundingClientRect().height));
    try {
      await getCurrentWindow().setSize(new LogicalSize(POPOVER_WIDTH, h));
    } catch {
      /* best-effort: a transient sizing failure self-corrects on the next refresh */
    }
  }

  /** Maps a band to its CSS class for bar fill + percent label. */
  function bandClass(band: Band): string {
    return `band-${band}`;
  }

  async function refreshUsage() {
    try {
      const [config, snapshot] = await Promise.all([getUsageLimitsConfig(), getUsageStatus()]);
      usageEnabled = config.enabled;
      usageSnapshot = snapshot;
    } catch {
      // fail silent — usage block just hides
    }
  }

  async function refreshPaused() {
    try {
      paused = (await getCaptureStatus()).paused;
    } catch (err) {
      errorMessage = String(err);
    }
  }

  async function resume() {
    try {
      paused = (await setCapturePaused(false)).paused;
    } catch (err) {
      errorMessage = String(err);
    }
  }

  // Task 5.1: hand off to the desktop window (backend hides the popover and
  // flips the activation policy so the Dock icon appears).
  async function openApp() {
    try {
      await openMainWindow();
    } catch (err) {
      errorMessage = String(err);
    }
  }

  function setSparklineDays(days: (typeof SPARKLINE_RANGES)[number]) {
    if (days === sparklineDays) return;
    sparklineDays = days;
    void refresh(days);
  }

  const sparklineTotal = $derived((dailyCosts ?? []).reduce((sum, day) => sum + day.cost_usd, 0));

  function dayLabel(ms: number): string {
    return new Date(ms).toLocaleDateString(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
    });
  }

  $effect(() => {
    void refresh();
    void refreshPaused();
    void refreshUsage();
    // The popover window is shown/hidden, never reloaded: refresh whenever
    // it regains focus (tray click), and live whenever the backend pushes
    // an ingest event (debounced; replaces the task-4.2 poll).
    const onFocus = () => {
      void refresh();
      void refreshPaused();
      void refreshUsage();
    };
    window.addEventListener("focus", onFocus);

    let ingestTimer: ReturnType<typeof setTimeout> | undefined;
    const unlistenIngested = listen(INGESTED_EVENT, () => {
      clearTimeout(ingestTimer);
      ingestTimer = setTimeout(() => void refresh(), INGEST_REFRESH_DEBOUNCE_MS);
    });
    const unlistenPaused = listen<CaptureStatus>(PAUSED_CHANGED_EVENT, (event) => {
      paused = event.payload.paused;
    });
    // Refetch budget status when config changes and on the coarse 60s tick
    // (covers month rollover); live spend is already covered by INGESTED.
    const unlistenBudgetConfig = listen(BUDGET_CONFIG_CHANGED, () => void refresh());
    const unlistenMetricsTick = listen(METRICS_TICK_EVENT, () => void refresh());
    const unlistenUsage = onUsageUpdated((snap) => {
      usageSnapshot = snap;
    });
    const unlistenMode = onDisplayModeChanged(() => {
      void refreshUsage();
    });

    // Keep the window sized to content across late layout (budget section
    // appearing, font/metric loads) so the footer is never clipped.
    let resizeObserver: ResizeObserver | undefined;
    if (popoverEl) {
      resizeObserver = new ResizeObserver(() => void resizeToContent());
      resizeObserver.observe(popoverEl);
    }

    return () => {
      window.removeEventListener("focus", onFocus);
      clearTimeout(ingestTimer);
      resizeObserver?.disconnect();
      void unlistenIngested.then((unlisten) => unlisten());
      void unlistenPaused.then((unlisten) => unlisten());
      void unlistenBudgetConfig.then((unlisten) => unlisten());
      void unlistenMetricsTick.then((unlisten) => unlisten());
      void unlistenUsage.then((stop) => stop());
      void unlistenMode.then((stop) => stop());
    };
  });
</script>

<main class="popover" bind:this={popoverEl}>
  <header>
    <div class="title">
      <img class="app-icon" src={farthingIcon} alt="" />
      <h1>Today</h1>
    </div>
    {#if metrics}
      <span class="muted">{dayLabel(metrics.day_start_ms)}</span>
    {/if}
  </header>

  {#if paused}
    <div class="paused-banner" role="status">
      <span class="paused-badge">Capture paused</span>
      <button type="button" class="resume-button" onclick={() => void resume()}> Resume </button>
    </div>
  {/if}

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {:else if !metrics}
    <p class="muted">Loading…</p>
  {:else}
    <section class="cost">
      <span class="cost-value">{formatCost(metrics.cost_usd)}</span>
      <span class="cost-label muted">API-equivalent</span>
    </section>
    {#if usageEnabled && usageSnapshot && (usageSnapshot.five_hour.percent !== null || usageSnapshot.seven_day.percent !== null)}
      <section class="usage-windows">
        <h2 class="muted">Plan usage</h2>
        {#each [usageSnapshot.five_hour, usageSnapshot.seven_day] as win (win.label)}
          {#if win.percent !== null}
            {@const pct = win.percent}
            {@const barClass =
              pct > 90 ? "usage-fill-danger" : pct > 75 ? "usage-fill-warn" : "usage-fill-ok"}
            <div class="usage-line">
              <div class="usage-row-head">
                <span class="usage-label">{win.label}</span>
                <span class="usage-pct {pct > 75 ? 'usage-pct-warn' : 'muted'}"
                  >{pct.toFixed(0)}%</span
                >
              </div>
              <div class="usage-bar">
                <div class="usage-fill {barClass}" style="width: {Math.min(pct, 100)}%"></div>
              </div>
              {#if win.resets_at_ms}
                <span class="usage-reset muted">{formatResetIn(win.resets_at_ms)}</span>
              {/if}
            </div>
          {/if}
        {/each}
      </section>
    {/if}
    {#if metrics.unpriced_requests > 0}
      <p class="footnote">
        {metrics.unpriced_requests} request{metrics.unpriced_requests === 1 ? "" : "s"} with unknown pricing
        excluded from cost (tokens counted).
      </p>
    {/if}

    {#if budgetStatus?.daily || budgetStatus?.monthly}
      <section class="budgets">
        {#each [{ label: "Daily budget", line: budgetStatus.daily }, { label: "Monthly budget", line: budgetStatus.monthly }] as entry (entry.label)}
          {#if entry.line}
            {@const line = entry.line}
            <div class="budget-line">
              <div class="budget-row-head">
                <span class="budget-label">
                  {#if line.band === "amber" || line.band === "red"}
                    <span class="warn-glyph" aria-hidden="true">⚠</span>
                  {/if}
                  {entry.label}
                </span>
              </div>
              <div class="budget-bar" class:exceeded={line.exceeded}>
                <div
                  class="budget-fill {line.exceeded ? 'band-red' : bandClass(line.band)}"
                  style="width: {Math.min(line.percent, 100)}%"
                ></div>
              </div>
              <div class="budget-amounts">
                <span class="muted">
                  {formatCost(line.spent_priced_usd)} / {formatCost(line.amount_usd)}
                </span>
                {#if line.exceeded}
                  <span class="budget-percent exceeded">· {line.percent}%</span>
                  <span class="exceeded-tag">Exceeded</span>
                {:else}
                  <span class="budget-percent">· {line.percent}%</span>
                {/if}
              </div>
              {#if line.unpriced_requests > 0}
                <p class="footnote">
                  {line.unpriced_requests} request{line.unpriced_requests === 1 ? "" : "s"} with unknown
                  pricing excluded
                </p>
              {/if}
            </div>
          {/if}
        {/each}
      </section>
    {/if}

    <section class="tokens">
      <div class="token">
        <span class="token-value">{formatTokens(metrics.input_tokens)}</span>
        <span class="token-label muted">Input</span>
      </div>
      <div class="token">
        <span class="token-value">{formatTokens(metrics.output_tokens)}</span>
        <span class="token-label muted">Output</span>
      </div>
      <div class="token">
        <span class="token-value">{formatTokens(metrics.cache_read_tokens)}</span>
        <span class="token-label muted">Cache read</span>
      </div>
      <div class="token">
        <span class="token-value">{formatTokens(metrics.cache_creation_tokens)}</span>
        <span class="token-label muted">Cache write</span>
      </div>
    </section>

    <p class="sessions">
      {metrics.sessions} session{metrics.sessions === 1 ? "" : "s"}
      <span class="muted">
        · {metrics.requests} request{metrics.requests === 1 ? "" : "s"}
      </span>
    </p>

    <section class="trend">
      <div class="trend-header">
        <h2 class="muted">Last {sparklineDays} days</h2>
        <div class="trend-controls">
          <span class="trend-total muted">{formatCost(sparklineTotal)}</span>
          <div class="range-toggle" role="group" aria-label="Sparkline range">
            {#each SPARKLINE_RANGES as days (days)}
              <button
                type="button"
                class:active={sparklineDays === days}
                aria-pressed={sparklineDays === days}
                onclick={() => setSparklineDays(days)}
              >
                {days}d
              </button>
            {/each}
          </div>
        </div>
      </div>
      {#if dailyCosts}
        <Sparkline series={dailyCosts} />
        {#if sparklineTotal === 0}
          <p class="muted footnote">No cost in the last {sparklineDays} days.</p>
        {/if}
      {/if}
    </section>

    <section class="projects">
      <h2 class="muted">Top projects</h2>
      {#if metrics.top_projects.length === 0}
        <p class="muted">No usage yet today.</p>
      {:else}
        <ul>
          {#each metrics.top_projects as project (project.cwd)}
            <li title={project.cwd ?? undefined}>
              <span class="project-name">{projectName(project.cwd)}</span>
              <span class="project-cost">{formatCost(project.cost_usd)}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    {#if import.meta.env.DEV}
      <p class="footnote">fetched + rendered in {renderMs.toFixed(1)}ms (dev)</p>
    {/if}
  {/if}

  <footer class="open-app-row">
    <button type="button" class="open-app" onclick={() => void openApp()}> Open Farthing </button>
  </footer>
</main>

<style>
  :global(html, body) {
    margin: 0;
    overflow: hidden;
    background: transparent;
  }

  .popover {
    box-sizing: border-box;
    /* Sizes to its content; the window is then resized to match exactly
       (resizeToContent), so there's no transparent gap and nothing clips. */
    height: auto;
    padding: 0.9rem 1rem;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    font-size: 0.85rem;
    color: #1d1d1f;
    background: #f6f6f7;
    user-select: none;
    cursor: default;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    border-bottom: 1px solid rgba(0, 0, 0, 0.12);
    padding-bottom: 0.5rem;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 0.45rem;
  }

  .app-icon {
    width: 18px;
    height: 18px;
    object-fit: contain;
  }

  h1 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
  }

  h2 {
    margin: 0 0 0.3rem;
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .muted {
    color: #6b6b6b;
  }

  .error {
    color: #b42318;
    line-height: 1.4;
  }

  .paused-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    margin-top: 0.6rem;
    padding: 0.35rem 0.5rem;
    border-radius: 6px;
    background: rgba(255, 159, 10, 0.16);
  }

  .paused-badge {
    font-size: 0.75rem;
    font-weight: 600;
    color: #93530a;
  }

  .resume-button {
    appearance: none;
    border: none;
    margin: 0;
    padding: 0.15rem 0.55rem;
    border-radius: 5px;
    font: inherit;
    font-size: 0.72rem;
    font-weight: 600;
    color: #fff;
    background: #0a84ff;
    cursor: pointer;
  }

  .cost {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }

  .cost-value {
    font-size: 1.7rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .cost-label {
    font-size: 0.75rem;
  }

  .budgets {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    margin-top: 0.85rem;
  }

  .budget-line {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .budget-row-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .budget-label {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6b6b6b;
  }

  .warn-glyph {
    color: #93530a;
  }

  .budget-bar {
    height: 7px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.08);
    overflow: hidden;
  }

  .budget-fill {
    height: 100%;
    border-radius: 4px;
  }

  .budget-fill.band-green {
    background: rgba(26, 127, 55, 0.18);
  }

  .budget-fill.band-yellow {
    background: rgba(180, 142, 10, 0.18);
  }

  .budget-fill.band-amber {
    background: rgba(255, 159, 10, 0.18);
  }

  .budget-fill.band-red {
    background: rgba(180, 35, 24, 0.18);
  }

  .budget-amounts {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }

  .budget-percent {
    color: #6b6b6b;
  }

  .budget-percent.exceeded {
    font-weight: 700;
    color: #b42318;
  }

  .exceeded-tag {
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.05rem 0.3rem;
    border-radius: 4px;
    color: #b42318;
    background: rgba(180, 35, 24, 0.14);
  }

  .tokens {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.45rem 0.75rem;
    margin-top: 0.85rem;
  }

  .token {
    display: flex;
    flex-direction: column;
    line-height: 1.25;
  }

  .token-value {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .token-label {
    font-size: 0.72rem;
  }

  .sessions {
    margin: 0.85rem 0 0;
  }

  .trend {
    margin-top: 0.85rem;
  }

  .trend-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
    margin-bottom: 0.3rem;
  }

  .trend-controls {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
  }

  .trend-total {
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }

  .range-toggle {
    display: inline-flex;
    border: 1px solid rgba(0, 0, 0, 0.15);
    border-radius: 5px;
    overflow: hidden;
  }

  .range-toggle button {
    appearance: none;
    border: none;
    margin: 0;
    padding: 0.1rem 0.45rem;
    font: inherit;
    font-size: 0.68rem;
    color: #6b6b6b;
    background: transparent;
    cursor: pointer;
  }

  .range-toggle button + button {
    border-left: 1px solid rgba(0, 0, 0, 0.15);
  }

  .range-toggle button.active {
    color: #fff;
    background: #0a84ff;
  }

  .projects {
    margin-top: 0.85rem;
  }

  .projects ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .projects li {
    display: flex;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.15rem 0;
  }

  .project-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .project-cost {
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .footnote {
    margin: 0.5rem 0 0;
    font-size: 0.7rem;
    color: #6b6b6b;
    line-height: 1.35;
  }

  .open-app-row {
    margin-top: 0.7rem;
    padding-top: 0.55rem;
    border-top: 1px solid rgba(0, 0, 0, 0.12);
  }

  .open-app {
    appearance: none;
    width: 100%;
    border: none;
    margin: 0;
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
    font: inherit;
    font-size: 0.76rem;
    font-weight: 600;
    color: #1d1d1f;
    background: rgba(0, 0, 0, 0.06);
    cursor: pointer;
  }

  .open-app:hover {
    background: rgba(0, 0, 0, 0.1);
  }

  .usage-windows {
    margin-top: 0.85rem;
  }

  .usage-line {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    margin-bottom: 0.5rem;
  }

  .usage-row-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }

  .usage-label {
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6b6b6b;
  }

  .usage-pct {
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }

  .usage-pct-warn {
    color: #b42318;
    font-weight: 700;
  }

  .usage-bar {
    height: 5px;
    border-radius: 3px;
    background: rgba(0, 0, 0, 0.08);
    overflow: hidden;
  }

  .usage-fill {
    height: 100%;
    border-radius: 3px;
  }

  .usage-fill-ok {
    background: rgba(26, 127, 55, 0.6);
  }
  .usage-fill-warn {
    background: rgba(180, 142, 10, 0.7);
  }
  .usage-fill-danger {
    background: rgba(180, 35, 24, 0.7);
  }

  .usage-reset {
    font-size: 0.66rem;
  }

  @media (prefers-color-scheme: dark) {
    .popover {
      color: #f5f5f7;
      background: #28282a;
    }

    header {
      border-bottom-color: rgba(255, 255, 255, 0.16);
    }

    .muted,
    .footnote {
      color: #9b9b9f;
    }

    .error {
      color: #ffa198;
    }

    .range-toggle {
      border-color: rgba(255, 255, 255, 0.22);
    }

    .range-toggle button {
      color: #9b9b9f;
    }

    .range-toggle button + button {
      border-left-color: rgba(255, 255, 255, 0.22);
    }

    .range-toggle button.active {
      color: #fff;
      background: #409cff;
    }

    .paused-banner {
      background: rgba(255, 159, 10, 0.22);
    }

    .paused-badge {
      color: #ffb55c;
    }

    .budget-label,
    .budget-percent {
      color: #9b9b9f;
    }

    .warn-glyph {
      color: #ffb55c;
    }

    .budget-bar {
      background: rgba(255, 255, 255, 0.12);
    }

    .budget-fill.band-green {
      background: rgba(126, 231, 135, 0.22);
    }

    .budget-fill.band-yellow {
      background: rgba(255, 181, 92, 0.22);
    }

    .budget-fill.band-amber {
      background: rgba(255, 159, 10, 0.22);
    }

    .budget-fill.band-red {
      background: rgba(255, 161, 152, 0.22);
    }

    .budget-percent.exceeded {
      color: #ffa198;
    }

    .exceeded-tag {
      color: #ffa198;
      background: rgba(255, 161, 152, 0.2);
    }

    .resume-button {
      background: #409cff;
    }

    .open-app-row {
      border-top-color: rgba(255, 255, 255, 0.16);
    }

    .open-app {
      color: #f5f5f7;
      background: rgba(255, 255, 255, 0.1);
    }

    .open-app:hover {
      background: rgba(255, 255, 255, 0.16);
    }

    .usage-label {
      color: #9b9b9f;
    }
    .usage-bar {
      background: rgba(255, 255, 255, 0.12);
    }
    .usage-fill-ok {
      background: rgba(126, 231, 135, 0.5);
    }
    .usage-fill-warn {
      background: rgba(255, 181, 92, 0.6);
    }
    .usage-fill-danger {
      background: rgba(255, 161, 152, 0.6);
    }
    .usage-pct-warn {
      color: #ffa198;
    }
  }
</style>
