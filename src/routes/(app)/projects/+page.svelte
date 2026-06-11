<script lang="ts">
  // Projects view (task 5.6): per-directory rollups (cost, tokens, requests,
  // session counts) under the global facets, sorted by cost descending
  // (pushed into SQL by `project_rollups`). Paths display cleaned — the
  // home-dir prefix becomes `~` (PRD FR-3) — and clicking a row applies
  // that project as the global project facet, so every other view inherits
  // it (clicking the active row clears it again). Sessions with no cwd
  // mapping roll up into "(unknown project)": data, not errors.
  import {
    getHomeDir,
    getProjectRollups,
    getUsageSummary,
    toFacets,
    type ProjectRollup,
    type UsageSummary,
  } from "$lib/queries";
  import { facets, UNKNOWN_PROJECT_OPTION } from "$lib/facets.svelte";
  import { cleanPath, formatCost, formatTokens, projectName } from "$lib/format";

  let rows: ProjectRollup[] | undefined = $state();
  let summary: UsageSummary | undefined = $state();
  let home: string | null = $state(null);
  let errorMessage = $state("");
  let loading = $state(true);
  let refreshKey = $state(0);
  let seq = 0;

  // The home dir never changes within a run: fetch once. A failure just
  // means paths display absolute (cleanPath passes them through).
  getHomeDir().then(
    (value) => {
      home = value;
    },
    () => {
      home = null;
    }
  );

  $effect(() => {
    void refreshKey;
    const params = toFacets(facets);
    const token = ++seq;
    loading = true;
    Promise.all([getProjectRollups(params), getUsageSummary(params)]).then(
      ([nextRows, nextSummary]) => {
        if (token !== seq) return;
        [rows, summary] = [nextRows, nextSummary];
        errorMessage = "";
        loading = false;
      },
      (err) => {
        if (token !== seq) return;
        errorMessage = String(err);
        loading = false;
      }
    );
  });

  // The window is hidden on close, never reloaded: refetch on focus so a
  // reopened window shows current data.
  $effect(() => {
    const onFocus = () => {
      refreshKey += 1;
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });

  /** The global project-facet value selecting this row's project. */
  function facetValue(row: ProjectRollup): string {
    return row.cwd ?? UNKNOWN_PROJECT_OPTION;
  }

  function isActive(row: ProjectRollup): boolean {
    return facets.project === facetValue(row);
  }

  /** Click-through: apply this project as the global facet (toggle off
   * when it's already the active filter). */
  function toggleProject(row: ProjectRollup) {
    facets.project = isActive(row) ? "" : facetValue(row);
  }

  function totalTokens(row: ProjectRollup): number {
    return row.input_tokens + row.output_tokens + row.cache_read_tokens + row.cache_creation_tokens;
  }

  function tokenBreakdown(row: ProjectRollup): string {
    return (
      `in ${row.input_tokens.toLocaleString()} · out ${row.output_tokens.toLocaleString()}` +
      ` · cache read ${row.cache_read_tokens.toLocaleString()}` +
      ` · cache write ${row.cache_creation_tokens.toLocaleString()}`
    );
  }

  /** The rollups partition every matching request, so the page total
   * reconciles with the summary (asserted by the dev footnote). */
  const pageCost = $derived((rows ?? []).reduce((sum, row) => sum + row.cost_usd, 0));
  const maxCost = $derived(Math.max(...(rows ?? []).map((row) => row.cost_usd), 0));
  const isEmpty = $derived(rows !== undefined && rows.length === 0);

  function costShare(row: ProjectRollup): number | null {
    if (pageCost <= 0) return null;
    return row.cost_usd / pageCost;
  }
</script>

<div class="projects-view">
  <header class="view-header">
    <h1>Projects</h1>
    {#if summary && rows}
      <span class="muted header-stats">
        {rows.length} project{rows.length === 1 ? "" : "s"} ·
        {summary.sessions} session{summary.sessions === 1 ? "" : "s"} ·
        {formatCost(summary.cost_usd)} API-equivalent
      </span>
    {/if}
  </header>

  {#if summary && summary.unpriced_requests > 0}
    <p class="footnote">
      {summary.unpriced_requests} request{summary.unpriced_requests === 1 ? "" : "s"} with unknown pricing
      excluded from cost (tokens counted).
    </p>
  {/if}

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {:else if !rows}
    <p class="muted">Loading…</p>
  {:else if isEmpty}
    <div class="empty">
      <p class="empty-title">No projects in this range</p>
      <p class="muted">Widen the date range or clear the active facets to see data.</p>
    </div>
  {:else}
    <div class="table-card" class:stale={loading}>
      <table>
        <thead>
          <tr>
            <th>Project</th>
            <th class="num">Sessions</th>
            <th class="num">Requests</th>
            <th class="num">Tokens</th>
            <th class="num" aria-sort="descending">Cost ▾</th>
            <th class="share-col">Share of cost</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as row (row.cwd ?? "")}
            <tr
              class="project-row"
              class:active={isActive(row)}
              class:unknown-project={row.cwd === null}
              role="button"
              tabindex="0"
              aria-pressed={isActive(row)}
              title={isActive(row)
                ? "Clear the project filter"
                : "Filter every view to this project"}
              onclick={() => toggleProject(row)}
              onkeydown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  toggleProject(row);
                }
              }}
            >
              <td class="project">
                <span class="project-name">{projectName(row.cwd)}</span>
                {#if isActive(row)}
                  <span class="chip active-chip">filtered</span>
                {/if}
                {#if row.cwd !== null}
                  <span class="project-path mono" title={row.cwd}>{cleanPath(row.cwd, home)}</span>
                {:else}
                  <span class="project-path">no cwd mapping recorded</span>
                {/if}
              </td>
              <td class="num">{row.sessions.toLocaleString()}</td>
              <td class="num">{row.requests.toLocaleString()}</td>
              <td class="num" title={tokenBreakdown(row)}>{formatTokens(totalTokens(row))}</td>
              <td class="num cost">
                {formatCost(row.cost_usd)}
                {#if row.unpriced_requests > 0}
                  <span
                    class="chip"
                    title="{row.unpriced_requests} request{row.unpriced_requests === 1
                      ? ''
                      : 's'} with unknown pricing (tokens counted, cost excluded)"
                  >
                    ~
                  </span>
                {/if}
              </td>
              <td class="share-col">
                {#if costShare(row) !== null}
                  <div class="share">
                    <div class="share-track">
                      <div
                        class="share-fill"
                        style:width="{maxCost > 0 ? (row.cost_usd / maxCost) * 100 : 0}%"
                      ></div>
                    </div>
                    <span class="share-pct">{(costShare(row)! * 100).toFixed(1)}%</span>
                  </div>
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if import.meta.env.DEV && summary}
      <p class="footnote reconcile">
        dev reconcile: summary ${summary.cost_usd.toFixed(6)} / {summary.sessions} sessions · page Σ ${pageCost.toFixed(
          6
        )} / Σ sessions {rows.reduce((sum, row) => sum + row.sessions, 0)}
      </p>
    {/if}
  {/if}
</div>

<style>
  .projects-view {
    max-width: 58rem;
  }

  .view-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 1rem;
  }

  h1 {
    margin: 0;
    font-size: 1.15rem;
    font-weight: 650;
  }

  .muted {
    color: #6b6b6b;
  }

  .mono {
    font-family: ui-monospace, monospace;
  }

  .error {
    color: #b42318;
    line-height: 1.4;
  }

  .header-stats {
    font-size: 0.82rem;
  }

  .stale {
    opacity: 0.6;
    transition: opacity 0.15s ease 0.15s;
  }

  .table-card {
    margin-top: 0.9rem;
    border: 1px solid rgba(0, 0, 0, 0.1);
    border-radius: 10px;
    background: #ffffff;
    overflow: hidden;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
  }

  th {
    text-align: left;
    font-weight: 600;
    font-size: 0.72rem;
    color: #6b6b6b;
    padding: 0.5rem 0.65rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.1);
    white-space: nowrap;
  }

  th.num,
  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  td {
    padding: 0.45rem 0.65rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
    vertical-align: baseline;
  }

  .project-row {
    cursor: pointer;
  }

  .project-row:hover,
  .project-row.active {
    background: rgba(10, 132, 255, 0.06);
  }

  .project-row:focus-visible {
    outline: 2px solid #0a84ff;
    outline-offset: -2px;
  }

  .project {
    max-width: 24rem;
  }

  .project-name {
    font-weight: 600;
  }

  .project-path {
    display: block;
    font-size: 0.72rem;
    color: #6b6b6b;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .unknown-project .project-name {
    color: #6b6b6b;
    font-style: italic;
    font-weight: 500;
  }

  .cost {
    font-weight: 600;
  }

  .chip {
    display: inline-block;
    padding: 0.05rem 0.35rem;
    border-radius: 999px;
    font-size: 0.65rem;
    font-weight: 600;
    background: rgba(0, 0, 0, 0.07);
    color: #4b4b4d;
    vertical-align: 0.08rem;
  }

  .active-chip {
    background: rgba(10, 132, 255, 0.14);
    color: #0a6ad1;
  }

  .share-col {
    width: 9.5rem;
  }

  .share {
    display: flex;
    align-items: center;
    gap: 0.45rem;
  }

  .share-track {
    flex: 1;
    height: 0.4rem;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.07);
    overflow: hidden;
  }

  .share-fill {
    height: 100%;
    border-radius: 999px;
    background: #0a84ff;
  }

  .share-pct {
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
    color: #6b6b6b;
    min-width: 2.8rem;
    text-align: right;
  }

  .empty {
    margin-top: 1rem;
    padding: 2.2rem 1.1rem;
    border: 1px dashed rgba(0, 0, 0, 0.18);
    border-radius: 10px;
    text-align: center;
  }

  .empty-title {
    margin: 0 0 0.3rem;
    font-weight: 600;
  }

  .empty .muted {
    margin: 0;
  }

  .footnote {
    margin: 0.5rem 0 0;
    font-size: 0.7rem;
    color: #6b6b6b;
    line-height: 1.35;
  }

  @media (prefers-color-scheme: dark) {
    .muted,
    .footnote,
    .project-path,
    .share-pct {
      color: #9b9b9f;
    }

    .error {
      color: #ffa198;
    }

    .table-card {
      background: #1f1f21;
      border-color: rgba(255, 255, 255, 0.12);
    }

    th {
      color: #9b9b9f;
      border-bottom-color: rgba(255, 255, 255, 0.12);
    }

    td {
      border-bottom-color: rgba(255, 255, 255, 0.07);
    }

    .project-row:hover,
    .project-row.active {
      background: rgba(64, 156, 255, 0.1);
    }

    .unknown-project .project-name {
      color: #9b9b9f;
    }

    .chip {
      background: rgba(255, 255, 255, 0.12);
      color: #cfcfd2;
    }

    .active-chip {
      background: rgba(64, 156, 255, 0.2);
      color: #8cc1ff;
    }

    .share-track {
      background: rgba(255, 255, 255, 0.12);
    }

    .share-fill {
      background: #409cff;
    }

    .empty {
      border-color: rgba(255, 255, 255, 0.22);
    }
  }
</style>
