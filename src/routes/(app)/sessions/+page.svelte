<script lang="ts">
  // Sessions view (task 5.4): per-session rollups under the global facets,
  // sortable by start time, duration, tokens, and cost (sorting and paging
  // are pushed into SQL by `session_rollups`); clicking a row drills into
  // `session_detail` — the per-request timeline, model mix, cache behavior,
  // and source tags for that session under the same facets, so the panel
  // always reconciles with the row that was clicked. Sessions with no cwd
  // mapping render as "(unknown project)": data, not errors (PRD FR-3).
  import {
    getSessionRollups,
    getSessionDetail,
    getUsageSummary,
    toFacets,
    type Facets,
    type ModelMix,
    type SessionDetail,
    type SessionRollup,
    type SessionSort,
    type UsageSummary,
  } from "$lib/queries";
  import { facets } from "$lib/facets.svelte";
  import {
    formatCost,
    formatDate,
    formatDateTime,
    formatDuration,
    formatTime,
    formatTokens,
    projectName,
  } from "$lib/format";
  import { page } from "$app/state";
  import { isExporting, runExport, type PreparedExport } from "$lib/export.svelte";
  import {
    buildReportHtml,
    buildSummaryCsv,
    type AggregatedCsv,
    type ReportFilter,
    type ReportTotals,
  } from "$lib/report/buildReport";

  /** Page size; "Load more" appends the next SQL page. */
  const PAGE = 100;

  const SORTABLE = [
    { key: "start", label: "Start" },
    { key: "duration", label: "Duration" },
    { key: "tokens", label: "Tokens" },
    { key: "cost", label: "Cost" },
  ] as const satisfies readonly { key: SessionSort; label: string }[];

  let sort: SessionSort = $state("start");
  let descending = $state(true);
  let rows: SessionRollup[] | undefined = $state();
  let summary: UsageSummary | undefined = $state();
  let moreAvailable = $state(false);
  let loadingMore = $state(false);
  let errorMessage = $state("");
  let loading = $state(true);
  let refreshKey = $state(0);
  let seq = 0;

  let expandedId: string | null = $state(null);
  let detail: SessionDetail | undefined = $state();
  let detailError = $state("");
  let detailLoading = $state(false);
  let detailSeq = 0;

  $effect(() => {
    void refreshKey;
    const params = toFacets(facets);
    const options = { sort, descending, limit: PAGE, offset: 0 };
    const token = ++seq;
    loading = true;
    Promise.all([getSessionRollups(params, options), getUsageSummary(params)]).then(
      ([page, nextSummary]) => {
        if (token !== seq) return;
        [rows, summary] = [page, nextSummary];
        moreAvailable = page.length === PAGE;
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

  // Fetch the drill-in whenever the expanded session or the facets change;
  // the detail applies the same facets as the table.
  $effect(() => {
    const id = expandedId;
    if (id === null) {
      detail = undefined;
      detailError = "";
      return;
    }
    const params = toFacets(facets);
    const token = ++detailSeq;
    detailLoading = true;
    getSessionDetail(id, params).then(
      (next) => {
        if (token !== detailSeq) return;
        detail = next;
        detailError = "";
        detailLoading = false;
      },
      (err) => {
        if (token !== detailSeq) return;
        detailError = String(err);
        detailLoading = false;
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

  function setSort(key: SessionSort) {
    if (sort === key) {
      descending = !descending;
    } else {
      sort = key;
      descending = true;
    }
  }

  function ariaSort(key: SessionSort): "ascending" | "descending" | undefined {
    if (sort !== key) return undefined;
    return descending ? "descending" : "ascending";
  }

  function toggleRow(sessionId: string) {
    expandedId = expandedId === sessionId ? null : sessionId;
  }

  function loadMore() {
    if (!rows || loadingMore) return;
    const params = toFacets(facets);
    const options = { sort, descending, limit: PAGE, offset: rows.length };
    const token = seq; // a list refetch invalidates this append
    loadingMore = true;
    getSessionRollups(params, options).then(
      (page) => {
        if (token !== seq) return;
        rows = [...(rows ?? []), ...page];
        moreAvailable = page.length === PAGE;
        loadingMore = false;
      },
      (err) => {
        if (token !== seq) return;
        errorMessage = String(err);
        loadingMore = false;
      }
    );
  }

  function totalTokens(row: SessionRollup): number {
    return row.input_tokens + row.output_tokens + row.cache_read_tokens + row.cache_creation_tokens;
  }

  function tokenBreakdown(row: SessionRollup): string {
    return (
      `in ${row.input_tokens.toLocaleString()} · out ${row.output_tokens.toLocaleString()}` +
      ` · cache read ${row.cache_read_tokens.toLocaleString()}` +
      ` · cache write ${row.cache_creation_tokens.toLocaleString()}`
    );
  }

  /** Cache hit rate (cache_read / (cache_read + input)); null when the
   * session never offered the cache a chance. Same definition as 5.5. */
  function cacheHitRate(read: number, input: number): number | null {
    return read + input === 0 ? null : read / (read + input);
  }

  const detailTotals = $derived.by(() => {
    const current = detail;
    if (!current) return undefined;
    const sum = (pick: (m: ModelMix) => number) =>
      current.models.reduce((total, mix) => total + pick(mix), 0);
    return {
      cost: sum((m) => m.cost_usd),
      input: sum((m) => m.input_tokens),
      cacheRead: sum((m) => m.cache_read_tokens),
      cacheCreation: sum((m) => m.cache_creation_tokens),
    };
  });

  const pageCost = $derived((rows ?? []).reduce((sum, row) => sum + row.cost_usd, 0));
  const isEmpty = $derived(rows !== undefined && rows.length === 0);

  // ---- export (R1/R2/R4/R16) ----
  //
  // Sessions is a TABLE report (no chart). Two fidelity rules:
  //  - Aggregated CSV = the FULL session-rollup set across ALL pages in the
  //    current sort (R16), not the visible/loaded page. The table only holds
  //    the pages the user scrolled, so the export re-queries the full set by
  //    paging at MAX_SESSION_LIMIT until a short page.
  //  - Raw CSV restricts to the session row set (`excludeSessionless=true`)
  //    so the raw rows match the session-rollup window (R9/R16); Rust orders
  //    that view's raw CSV by `session_id, timestamp_ms`.
  //
  // `session_rollups` clamps any limit to MAX_SESSION_LIMIT (2000) server-side
  // (queries.rs); page through it the same way.
  const ALL_PAGE = 2000;

  /** Fetch every session rollup for the facets in the given sort, paging until
   * a short page. Pure over its inputs (the consistent snapshotted facets). */
  async function fetchAllRollups(
    snapFacets: Facets,
    snapSort: SessionSort,
    snapDescending: boolean
  ): Promise<SessionRollup[]> {
    const all: SessionRollup[] = [];
    for (let offset = 0; ; offset += ALL_PAGE) {
      const batch = await getSessionRollups(snapFacets, {
        sort: snapSort,
        descending: snapDescending,
        limit: ALL_PAGE,
        offset,
      });
      all.push(...batch);
      if (batch.length < ALL_PAGE) break;
    }
    return all;
  }

  const SORT_LABEL: Record<SessionSort, string> = {
    start: "Start",
    duration: "Duration",
    tokens: "Tokens",
    cost: "Cost",
  };

  /** Identity-header filter chips (R6): source, project, model, plus the
   * active sort (the table report's "grouping" analogue). */
  function reportFilters(
    snapFacets: Facets,
    snapSort: SessionSort,
    snapDescending: boolean
  ): ReportFilter[] {
    const filters: ReportFilter[] = [];
    if (snapFacets.query_source && snapFacets.query_source !== "all") {
      filters.push({ label: "Source", value: snapFacets.query_source });
    }
    const project = snapFacets.project;
    if (project === "unknown") {
      filters.push({ label: "Project", value: "(unknown)" });
    } else if (project && typeof project === "object") {
      filters.push({ label: "Project", value: projectName(project.cwd) });
    }
    if (snapFacets.model) {
      filters.push({ label: "Model", value: snapFacets.model });
    }
    filters.push({
      label: "Sorted by",
      value: `${SORT_LABEL[snapSort]} ${snapDescending ? "↓" : "↑"}`,
    });
    return filters;
  }

  /** Aggregated CSV/table datapoints (R7/R16): one row per session rollup, in
   * the current sort, across all pages. */
  function aggregatedFromRollups(all: SessionRollup[]): AggregatedCsv {
    return {
      columns: [
        "Session",
        "Project",
        "Start",
        "Duration",
        "Models",
        "Requests",
        "Errors",
        "Tokens",
        "Cost (USD)",
      ],
      rows: all.map((row) => [
        row.session_id,
        projectName(row.cwd),
        formatDateTime(row.first_ms),
        formatDuration(row.last_ms - row.first_ms),
        row.models.join(" "),
        String(row.requests),
        String(row.errors),
        String(totalTokens(row)),
        String(row.cost_usd),
      ]),
    };
  }

  /** Reconciliation totals for the report (R9): from the consistent read. */
  function reportTotals(s: UsageSummary): ReportTotals {
    return {
      costUsd: s.cost_usd,
      requests: s.requests,
      unpricedRequests: s.unpriced_requests,
      errors: s.errors,
      inputTokens: s.input_tokens,
      outputTokens: s.output_tokens,
      cacheReadTokens: s.cache_read_tokens,
      cacheCreationTokens: s.cache_creation_tokens,
    };
  }

  function windowLabel(s: UsageSummary): string {
    if (s.start_ms === null || s.end_ms === null) return "All time";
    return `${formatDate(s.start_ms)} – ${formatDate(s.end_ms - 1)}`;
  }

  function onExport(): void {
    // Synchronous snapshot (R2/R4): capture facets, the active sort, and the
    // originating route before any await, so re-sorting after the click can't
    // change the produced bundle.
    const snapFacets = toFacets(facets);
    const snapSort = sort;
    const snapDescending = descending;
    const originRoute = page.url.pathname;

    void runExport({
      view: "sessions",
      facets: snapFacets,
      originRoute,
      prepare: async (): Promise<PreparedExport> => {
        // Consistent point-in-time read (R9): summary for counts/totals, and
        // the FULL rollup set across all pages for the aggregated CSV (R16) —
        // NOT the view's loaded `$state`, which only holds visible pages.
        const [snapSummary, allRollups] = await Promise.all([
          getUsageSummary(snapFacets),
          fetchAllRollups(snapFacets, snapSort, snapDescending),
        ]);

        if (snapSummary.requests === 0 && snapSummary.errors === 0) {
          return {
            requests: 0,
            errors: 0,
            reportHtml: "",
            summaryCsv: "",
            // Sessions raw CSV = session row set (R16): excludes session-less rows.
            excludeSessionless: true,
          };
        }

        const aggregated = aggregatedFromRollups(allRollups);
        const reportHtml = buildReportHtml({
          title: "Sessions",
          rangeLabel: windowLabel(snapSummary),
          filters: reportFilters(snapFacets, snapSort, snapDescending),
          totals: reportTotals(snapSummary),
          // Table report: no chart.
          aggregated,
          generatedAtMs: Date.now(),
        });

        return {
          requests: snapSummary.requests,
          errors: snapSummary.errors,
          reportHtml,
          summaryCsv: buildSummaryCsv(aggregated),
          // R16: raw CSV uses the session row set + `session_id, timestamp_ms`
          // order, which the Rust export applies when this flag is true.
          excludeSessionless: true,
        };
      },
    });
  }
</script>

<div class="sessions-view">
  <header class="view-header">
    <h1>Sessions</h1>
    <div class="header-actions">
      {#if summary}
        <span class="muted header-stats">
          {summary.sessions} session{summary.sessions === 1 ? "" : "s"} ·
          {formatCost(summary.cost_usd)} API-equivalent
        </span>
      {/if}
      <button
        type="button"
        class="export-button"
        onclick={onExport}
        disabled={isExporting() || loading}
      >
        Export
      </button>
    </div>
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
      <p class="empty-title">No sessions in this range</p>
      <p class="muted">Widen the date range or clear the active facets to see data.</p>
    </div>
  {:else}
    <div class="table-card" class:stale={loading}>
      <table>
        <thead>
          <tr>
            {#each SORTABLE.slice(0, 2) as column (column.key)}
              <th aria-sort={ariaSort(column.key)}>
                <button type="button" class="sort" onclick={() => setSort(column.key)}>
                  {column.label}
                  {#if sort === column.key}<span class="arrow">{descending ? "▾" : "▴"}</span>{/if}
                </button>
              </th>
            {/each}
            <th>Project</th>
            <th>Models</th>
            <th class="num">Requests</th>
            {#each SORTABLE.slice(2) as column (column.key)}
              <th class="num" aria-sort={ariaSort(column.key)}>
                <button type="button" class="sort" onclick={() => setSort(column.key)}>
                  {column.label}
                  {#if sort === column.key}<span class="arrow">{descending ? "▾" : "▴"}</span>{/if}
                </button>
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each rows as row (row.session_id)}
            <tr
              class="session-row"
              class:expanded={expandedId === row.session_id}
              class:unknown-project={row.cwd === null}
              role="button"
              tabindex="0"
              aria-expanded={expandedId === row.session_id}
              onclick={() => toggleRow(row.session_id)}
              onkeydown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  toggleRow(row.session_id);
                }
              }}
            >
              <td class="nowrap">{formatDateTime(row.first_ms)}</td>
              <td class="nowrap">{formatDuration(row.last_ms - row.first_ms)}</td>
              <td class="project" title={row.cwd ?? undefined}>{projectName(row.cwd)}</td>
              <td class="models" title={row.models.join(", ")}>{row.models.join(", ")}</td>
              <td class="num">
                {row.requests.toLocaleString()}
                {#if row.errors > 0}
                  <span
                    class="chip error-chip"
                    title="{row.errors} api_error row{row.errors === 1 ? '' : 's'}"
                  >
                    {row.errors} err
                  </span>
                {/if}
              </td>
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
            </tr>
            {#if expandedId === row.session_id}
              <tr class="detail-row">
                <td colspan="7">
                  {#if detailError}
                    <p class="error">{detailError}</p>
                  {:else if !detail || detail.session_id !== row.session_id}
                    <p class="muted">Loading session detail…</p>
                  {:else}
                    <div class="detail" class:stale={detailLoading}>
                      <div class="detail-header">
                        <span class="detail-project">{detail.cwd ?? "(unknown project)"}</span>
                        <span class="muted mono">{detail.session_id}</span>
                      </div>

                      <div class="detail-panels">
                        <section class="panel">
                          <h2>Model mix</h2>
                          <ul class="mix">
                            {#each detail.models as mix (mix.model ?? "")}
                              <li>
                                <span class="mix-model" title={mix.model ?? undefined}>
                                  {mix.model ?? "(no model)"}
                                </span>
                                <span class="muted">
                                  {mix.requests.toLocaleString()} req ·
                                  {formatTokens(
                                    mix.input_tokens +
                                      mix.output_tokens +
                                      mix.cache_read_tokens +
                                      mix.cache_creation_tokens
                                  )} tok
                                </span>
                                <span class="mix-cost">{formatCost(mix.cost_usd)}</span>
                              </li>
                            {/each}
                          </ul>
                        </section>

                        {#if detailTotals}
                          <section class="panel">
                            <h2>Cache behavior</h2>
                            <dl class="cache">
                              <dt>Hit rate</dt>
                              <dd>
                                {#if cacheHitRate(detailTotals.cacheRead, detailTotals.input) !== null}
                                  {(
                                    cacheHitRate(detailTotals.cacheRead, detailTotals.input)! * 100
                                  ).toFixed(0)}%
                                  <span class="muted">(cache read / (cache read + input))</span>
                                {:else}
                                  <span class="muted">no cacheable input</span>
                                {/if}
                              </dd>
                              <dt>Cache read</dt>
                              <dd>{formatTokens(detailTotals.cacheRead)} tokens</dd>
                              <dt>Cache write</dt>
                              <dd>
                                {formatTokens(detailTotals.cacheCreation)} tokens
                                {#if detail.requests.some((r) => r.cache_creation_5m_tokens !== null || r.cache_creation_1h_tokens !== null)}
                                  <span class="muted">
                                    ({formatTokens(
                                      detail.requests.reduce(
                                        (sum, r) => sum + (r.cache_creation_5m_tokens ?? 0),
                                        0
                                      )
                                    )} 5m ·
                                    {formatTokens(
                                      detail.requests.reduce(
                                        (sum, r) => sum + (r.cache_creation_1h_tokens ?? 0),
                                        0
                                      )
                                    )} 1h)
                                  </span>
                                {/if}
                              </dd>
                            </dl>
                          </section>
                        {/if}
                      </div>

                      <section class="timeline">
                        <h2>
                          Request timeline
                          {#if detail.total_rows > detail.requests.length}
                            <span class="muted">
                              (first {detail.requests.length.toLocaleString()} of
                              {detail.total_rows.toLocaleString()})
                            </span>
                          {/if}
                        </h2>
                        <table class="timeline-table">
                          <thead>
                            <tr>
                              <th>Time</th>
                              <th>Model</th>
                              <th>Source</th>
                              <th class="num">In</th>
                              <th class="num">Out</th>
                              <th class="num">Cache read</th>
                              <th class="num">Cache write</th>
                              <th class="num">Cost</th>
                            </tr>
                          </thead>
                          <tbody>
                            {#each detail.requests as request, i (i)}
                              <tr class:error-row={request.event_type === "api_error"}>
                                <td class="nowrap" title={formatDateTime(request.timestamp_ms)}>
                                  {formatTime(request.timestamp_ms)}
                                </td>
                                <td class="models" title={request.model ?? undefined}>
                                  {request.model ?? "—"}
                                  {#if request.event_type === "api_error"}
                                    <span
                                      class="chip error-chip"
                                      title={request.error ?? "api_error"}
                                    >
                                      error
                                    </span>
                                  {/if}
                                </td>
                                <td class="nowrap">
                                  <span class="chip source-chip"
                                    >{request.query_source ?? "main"}</span
                                  >
                                  <span class="chip data-chip">{request.source}</span>
                                </td>
                                <td class="num">{formatTokens(request.input_tokens)}</td>
                                <td class="num">{formatTokens(request.output_tokens)}</td>
                                <td class="num">{formatTokens(request.cache_read_tokens)}</td>
                                <td
                                  class="num"
                                  title={request.cache_creation_5m_tokens !== null ||
                                  request.cache_creation_1h_tokens !== null
                                    ? `5m ${request.cache_creation_5m_tokens ?? 0} · 1h ${request.cache_creation_1h_tokens ?? 0}`
                                    : undefined}
                                >
                                  {formatTokens(request.cache_creation_tokens)}
                                </td>
                                <td class="num cost">
                                  {#if request.cost_usd !== null}
                                    {formatCost(request.cost_usd)}
                                  {:else if request.event_type === "api_error"}
                                    <span class="muted">—</span>
                                  {:else}
                                    <span class="muted" title="Unknown model pricing">unpriced</span
                                    >
                                  {/if}
                                </td>
                              </tr>
                            {/each}
                          </tbody>
                        </table>
                      </section>
                    </div>
                  {/if}
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    </div>

    {#if moreAvailable}
      <button type="button" class="load-more" onclick={loadMore} disabled={loadingMore}>
        {loadingMore ? "Loading…" : "Load more"}
      </button>
    {/if}

    {#if import.meta.env.DEV && summary}
      <p class="footnote reconcile">
        dev reconcile: summary ${summary.cost_usd.toFixed(6)} / {summary.sessions} sessions · page Σ ${pageCost.toFixed(
          6
        )} / {rows.length} rows{moreAvailable ? " (partial page)" : ""}
      </p>
    {/if}
  {/if}
</div>

<style>
  .sessions-view {
    max-width: 64rem;
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

  h2 {
    margin: 0 0 0.4rem;
    font-size: 0.78rem;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: #6b6b6b;
  }

  .muted {
    color: #6b6b6b;
  }

  .mono {
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
  }

  .error {
    color: #b42318;
    line-height: 1.4;
  }

  .header-stats {
    font-size: 0.82rem;
  }

  .header-actions {
    display: inline-flex;
    align-items: baseline;
    gap: 0.7rem;
  }

  .export-button {
    appearance: none;
    border: 1px solid rgba(0, 0, 0, 0.15);
    border-radius: 6px;
    margin: 0;
    padding: 0.25rem 0.85rem;
    font: inherit;
    font-size: 0.76rem;
    color: #1c1c1e;
    background: #fff;
    cursor: pointer;
    align-self: center;
  }

  .export-button:hover:not(:disabled) {
    background: #f2f2f4;
  }

  .export-button:disabled {
    opacity: 0.5;
    cursor: default;
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

  th .sort {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    margin: 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }

  th .sort:hover {
    color: #1c1c1e;
  }

  .arrow {
    font-size: 0.65rem;
  }

  td {
    padding: 0.45rem 0.65rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
    vertical-align: baseline;
  }

  .session-row {
    cursor: pointer;
  }

  .session-row:hover,
  .session-row.expanded {
    background: rgba(10, 132, 255, 0.06);
  }

  .session-row:focus-visible {
    outline: 2px solid #0a84ff;
    outline-offset: -2px;
  }

  .nowrap {
    white-space: nowrap;
  }

  .project,
  .models {
    max-width: 13rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .unknown-project .project {
    color: #6b6b6b;
    font-style: italic;
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

  .error-chip {
    background: rgba(180, 35, 24, 0.12);
    color: #b42318;
  }

  .source-chip {
    background: rgba(10, 132, 255, 0.12);
    color: #0a6ad1;
  }

  .data-chip {
    background: rgba(0, 0, 0, 0.06);
  }

  .detail-row > td {
    padding: 0;
    background: rgba(0, 0, 0, 0.02);
  }

  .detail {
    padding: 0.85rem 0.9rem 1rem;
  }

  .detail-header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.6rem;
    margin-bottom: 0.7rem;
  }

  .detail-project {
    font-weight: 600;
    word-break: break-all;
  }

  .detail-panels {
    display: flex;
    flex-wrap: wrap;
    gap: 0.8rem;
    margin-bottom: 0.9rem;
  }

  .panel {
    flex: 1 1 16rem;
    padding: 0.6rem 0.75rem;
    border: 1px solid rgba(0, 0, 0, 0.08);
    border-radius: 8px;
    background: #ffffff;
  }

  .mix {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .mix li {
    display: flex;
    align-items: baseline;
    gap: 0.55rem;
    font-size: 0.78rem;
  }

  .mix-model {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mix-cost {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }

  .cache {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.8rem;
    margin: 0;
    font-size: 0.78rem;
  }

  .cache dt {
    color: #6b6b6b;
  }

  .cache dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }

  .timeline-table {
    font-size: 0.76rem;
  }

  .timeline-table th,
  .timeline-table td {
    padding: 0.3rem 0.5rem;
  }

  .timeline-table .models {
    max-width: 11rem;
  }

  .error-row td {
    color: #b42318;
  }

  .load-more {
    appearance: none;
    margin-top: 0.8rem;
    padding: 0.35rem 0.9rem;
    border: 1px solid rgba(0, 0, 0, 0.15);
    border-radius: 6px;
    background: transparent;
    font: inherit;
    font-size: 0.78rem;
    color: #1c1c1e;
    cursor: pointer;
  }

  .load-more:disabled {
    opacity: 0.6;
    cursor: default;
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
    h2,
    .cache dt {
      color: #9b9b9f;
    }

    .error {
      color: #ffa198;
    }

    .export-button {
      color: #e7e7ea;
      background: #2a2a2c;
      border-color: rgba(255, 255, 255, 0.22);
    }

    .export-button:hover:not(:disabled) {
      background: #333335;
    }

    .table-card,
    .panel {
      background: #1f1f21;
      border-color: rgba(255, 255, 255, 0.12);
    }

    th {
      color: #9b9b9f;
      border-bottom-color: rgba(255, 255, 255, 0.12);
    }

    th .sort:hover {
      color: #f2f2f4;
    }

    td {
      border-bottom-color: rgba(255, 255, 255, 0.07);
    }

    .session-row:hover,
    .session-row.expanded {
      background: rgba(64, 156, 255, 0.1);
    }

    .unknown-project .project {
      color: #9b9b9f;
    }

    .chip {
      background: rgba(255, 255, 255, 0.12);
      color: #cfcfd2;
    }

    .error-chip {
      background: rgba(255, 161, 152, 0.18);
      color: #ffa198;
    }

    .source-chip {
      background: rgba(64, 156, 255, 0.2);
      color: #8cc1ff;
    }

    .data-chip {
      background: rgba(255, 255, 255, 0.1);
    }

    .detail-row > td {
      background: rgba(255, 255, 255, 0.025);
    }

    .error-row td {
      color: #ffa198;
    }

    .load-more {
      border-color: rgba(255, 255, 255, 0.22);
      color: #f2f2f4;
    }

    .empty {
      border-color: rgba(255, 255, 255, 0.22);
    }
  }
</style>
