<script lang="ts">
  // Popover content (task 4.2): today's cost (API-equivalent), token split,
  // session count, and top 3 projects by cost. The window is created hidden
  // at startup and toggled by the tray icon (task 4.1); data refreshes on
  // focus (every open) and on a short poll while the page lives. Task 4.3
  // adds the sparkline, 4.4 swaps the poll for ingest-event push.
  import { tick } from "svelte";
  import { getTodayMetrics, type TodayMetrics } from "$lib/metrics";

  const REFRESH_INTERVAL_MS = 5_000;

  let metrics: TodayMetrics | undefined = $state();
  let errorMessage = $state("");
  /** Fetch + DOM update time of the last refresh (dev render budget check). */
  let renderMs = $state(0);

  async function refresh() {
    const started = performance.now();
    try {
      metrics = await getTodayMetrics();
      errorMessage = "";
      await tick();
      renderMs = performance.now() - started;
    } catch (err) {
      errorMessage = String(err);
    }
  }

  function formatCost(value: number): string {
    if (value === 0) return "$0.00";
    if (value < 0.01) return "<$0.01";
    if (value >= 1000) return `$${Math.round(value).toLocaleString()}`;
    if (value >= 100) return `$${value.toFixed(0)}`;
    return `$${value.toFixed(2)}`;
  }

  function formatTokens(value: number): string {
    if (value < 1_000) return String(value);
    if (value < 1_000_000) return `${(value / 1_000).toFixed(1)}k`;
    if (value < 1_000_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
    return `${(value / 1_000_000_000).toFixed(1)}B`;
  }

  /** Display name for a project: the last path segment of the session cwd. */
  function projectName(cwd: string | null): string {
    if (cwd === null) return "(unknown project)";
    const segments = cwd.split("/").filter((s) => s.length > 0);
    return segments[segments.length - 1] ?? cwd;
  }

  function dayLabel(ms: number): string {
    return new Date(ms).toLocaleDateString(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
    });
  }

  $effect(() => {
    void refresh();
    // The popover window is shown/hidden, never reloaded: refresh whenever
    // it regains focus (tray click) plus a poll for the time it stays open.
    const timer = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  });
</script>

<main class="popover">
  <header>
    <h1>Today</h1>
    {#if metrics}
      <span class="muted">{dayLabel(metrics.day_start_ms)}</span>
    {/if}
  </header>

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {:else if !metrics}
    <p class="muted">Loading…</p>
  {:else}
    <section class="cost">
      <span class="cost-value">{formatCost(metrics.cost_usd)}</span>
      <span class="cost-label muted">API-equivalent</span>
    </section>
    {#if metrics.unpriced_requests > 0}
      <p class="footnote">
        {metrics.unpriced_requests} request{metrics.unpriced_requests === 1 ? "" : "s"} with unknown pricing
        excluded from cost (tokens counted).
      </p>
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
</main>

<style>
  :global(html, body) {
    margin: 0;
    overflow: hidden;
    background: transparent;
  }

  .popover {
    box-sizing: border-box;
    height: 100vh;
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
    align-items: baseline;
    justify-content: space-between;
    border-bottom: 1px solid rgba(0, 0, 0, 0.12);
    padding-bottom: 0.5rem;
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
  }
</style>
