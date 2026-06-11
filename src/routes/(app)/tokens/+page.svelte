<script lang="ts">
  // Tokens & cache view (task 5.5): the four token counters per local day
  // under the global facets (same range controls as the cost view: the
  // shared FacetBar drives every analysis view), plus the cache hit-rate
  // trend. All charts draw from one ungrouped `usage_series` fetch, and the
  // headline totals come from the same `usage_summary` the other views
  // reconcile against, so the numbers can never disagree across views.
  //
  // Cache hit rate = cache read / (cache read + input) tokens: the share of
  // prompt tokens served from cache instead of being sent at full price.
  // The 5m/1h cache-creation split exists only on transcript-backfilled
  // rows (the OTel stream reports a single cache-creation counter), so the
  // cache-creation chart stacks 5m/1h where available and folds the
  // remainder into an "unsplit" segment.
  import {
    getUsageSeries,
    getUsageSummary,
    toFacets,
    type SeriesPoint,
    type UsageSummary,
  } from "$lib/queries";
  import { facets } from "$lib/facets.svelte";
  import { formatDate, formatDay, formatTokens } from "$lib/format";
  import StackedBarChart, {
    type ChartBucket,
    type ChartSegment,
  } from "$lib/StackedBarChart.svelte";

  const TOKEN_KINDS = [
    { key: "input_tokens", label: "Input", color: "#0a84ff" },
    { key: "output_tokens", label: "Output", color: "#30d158" },
    { key: "cache_read_tokens", label: "Cache read", color: "#bf5af2" },
    { key: "cache_creation_tokens", label: "Cache creation", color: "#ff9f0a" },
  ] as const;
  type TokenKey = (typeof TOKEN_KINDS)[number]["key"];

  const SPLIT_5M_COLOR = "#ff9f0a";
  const SPLIT_1H_COLOR = "#ff375f";
  const SPLIT_UNSPLIT_COLOR = "#8e8e93";
  const HIT_RATE_COLOR = "#64d2ff";

  let summary: UsageSummary | undefined = $state();
  let series: SeriesPoint[] | undefined = $state();
  let errorMessage = $state("");
  let loading = $state(true);
  let refreshKey = $state(0);
  let seq = 0;

  $effect(() => {
    void refreshKey;
    const params = toFacets(facets);
    const token = ++seq;
    loading = true;
    Promise.all([getUsageSummary(params), getUsageSeries(params, "none")]).then(
      ([nextSummary, nextSeries]) => {
        if (token !== seq) return;
        [summary, series] = [nextSummary, nextSeries];
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

  /** Hit rate for one bucket/total; null when no prompt tokens exist. */
  function hitRate(cacheRead: number, input: number): number | null {
    const denominator = cacheRead + input;
    if (denominator <= 0) return null;
    return cacheRead / denominator;
  }

  function formatRate(value: number): string {
    return `${(value * 100).toFixed(1)}%`;
  }

  /** One single-segment chart per token kind, same bucket skeleton. */
  function tokenBuckets(points: SeriesPoint[], kind: (typeof TOKEN_KINDS)[number]): ChartBucket[] {
    return points.map((point) => ({
      start_ms: point.bucket_start_ms,
      label: formatDay(point.bucket_start_ms),
      segments: [{ id: kind.key, label: kind.label, color: kind.color, value: point[kind.key] }],
    }));
  }

  /** Cache creation stacked 5m/1h where the (transcript-only) split exists;
   * the remainder is live-captured volume the stream doesn't split. */
  const cacheCreationBuckets = $derived.by((): ChartBucket[] => {
    if (!series) return [];
    return series.map((point) => {
      const split5m = point.cache_creation_5m_tokens ?? 0;
      const split1h = point.cache_creation_1h_tokens ?? 0;
      const unsplit = Math.max(point.cache_creation_tokens - split5m - split1h, 0);
      const segments: ChartSegment[] = [
        { id: "5m", label: "5m TTL", color: SPLIT_5M_COLOR, value: split5m },
        { id: "1h", label: "1h TTL", color: SPLIT_1H_COLOR, value: split1h },
        {
          id: "unsplit",
          label: "Unsplit (live capture)",
          color: SPLIT_UNSPLIT_COLOR,
          value: unsplit,
        },
      ];
      return { start_ms: point.bucket_start_ms, label: formatDay(point.bucket_start_ms), segments };
    });
  });

  const hasSplit = $derived(
    (series ?? []).some(
      (point) => point.cache_creation_5m_tokens !== null || point.cache_creation_1h_tokens !== null
    )
  );

  /** Hit-rate trend: one bar per day; days with no prompt tokens draw no
   * bar (a rate of zero is data, an empty day is not). */
  const hitRateBuckets = $derived.by((): ChartBucket[] => {
    if (!series) return [];
    return series.map((point) => {
      const rate = hitRate(point.cache_read_tokens, point.input_tokens);
      return {
        start_ms: point.bucket_start_ms,
        label: formatDay(point.bucket_start_ms),
        segments: [{ id: "rate", label: "Hit rate", color: HIT_RATE_COLOR, value: rate ?? 0 }],
      };
    });
  });

  const totalTokens = $derived(
    summary
      ? summary.input_tokens +
          summary.output_tokens +
          summary.cache_read_tokens +
          summary.cache_creation_tokens
      : 0
  );
  const overallHitRate = $derived(
    summary ? hitRate(summary.cache_read_tokens, summary.input_tokens) : null
  );
  const isEmpty = $derived(summary !== undefined && summary.requests === 0 && summary.errors === 0);

  function seriesSum(key: TokenKey): number {
    return (series ?? []).reduce((sum, point) => sum + point[key], 0);
  }

  function windowLabel(s: UsageSummary): string {
    if (s.start_ms === null || s.end_ms === null) return "All time";
    return `${formatDate(s.start_ms)} – ${formatDate(s.end_ms - 1)}`;
  }
</script>

<div class="tokens-view">
  <header class="view-header">
    <h1>Tokens & cache</h1>
  </header>

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {:else if !summary || !series}
    <p class="muted">Loading…</p>
  {:else}
    <section class="totals" class:stale={loading}>
      <span class="total-tokens">{formatTokens(totalTokens)}</span>
      <span class="muted">tokens</span>
      <span class="muted divider">·</span>
      <span class="muted">{windowLabel(summary)}</span>
      <span class="muted divider">·</span>
      <span class="muted">
        {summary.requests} request{summary.requests === 1 ? "" : "s"} across {summary.sessions}
        session{summary.sessions === 1 ? "" : "s"}
      </span>
      {#if overallHitRate !== null}
        <span class="muted divider">·</span>
        <span class="hit-rate-total">{formatRate(overallHitRate)} cache hit rate</span>
      {/if}
    </section>

    {#if isEmpty}
      <div class="empty">
        <p class="empty-title">No usage in this range</p>
        <p class="muted">Widen the date range or clear the active facets to see data.</p>
      </div>
    {:else}
      <div class="chart-grid" class:stale={loading}>
        {#each TOKEN_KINDS as kind (kind.key)}
          <section class="chart-card">
            <h2>
              <span class="swatch" style="background: {kind.color}"></span>
              {kind.label}
              <span class="card-total muted">{formatTokens(summary[kind.key])}</span>
            </h2>
            {#if kind.key === "cache_creation_tokens"}
              <StackedBarChart
                buckets={cacheCreationBuckets}
                formatValue={formatTokens}
                ariaLabel="Daily cache-creation tokens, {series.length} day{series.length === 1
                  ? ''
                  : 's'}"
              />
              {#if hasSplit}
                <p class="footnote">
                  <span class="swatch" style="background: {SPLIT_5M_COLOR}"></span> 5m TTL
                  {formatTokens(summary.cache_creation_5m_tokens ?? 0)}
                  <span class="swatch one-h" style="background: {SPLIT_1H_COLOR}"></span> 1h TTL
                  {formatTokens(summary.cache_creation_1h_tokens ?? 0)}
                  <span class="swatch one-h" style="background: {SPLIT_UNSPLIT_COLOR}"></span>
                  unsplit (live capture)
                </p>
              {:else}
                <p class="footnote">
                  5m/1h TTL split unavailable here: it comes from transcript-backfilled data only.
                </p>
              {/if}
            {:else}
              <StackedBarChart
                buckets={tokenBuckets(series, kind)}
                formatValue={formatTokens}
                ariaLabel="Daily {kind.label.toLowerCase()} tokens, {series.length} day{series.length ===
                1
                  ? ''
                  : 's'}"
              />
            {/if}
          </section>
        {/each}
      </div>

      <section class="chart-card hit-rate-card" class:stale={loading}>
        <h2>
          <span class="swatch" style="background: {HIT_RATE_COLOR}"></span>
          Cache hit rate
          {#if overallHitRate !== null}
            <span class="card-total muted">{formatRate(overallHitRate)} overall</span>
          {/if}
        </h2>
        <StackedBarChart
          buckets={hitRateBuckets}
          formatValue={formatRate}
          ariaLabel="Daily cache hit rate, {series.length} day{series.length === 1 ? '' : 's'}"
        />
        <p class="footnote">
          Cache hit rate = cache read ÷ (cache read + input) tokens: the share of prompt tokens
          served from cache. Days with no prompt tokens draw no bar.
        </p>
      </section>
    {/if}

    {#if import.meta.env.DEV}
      <p class="footnote reconcile">
        dev reconcile: summary in {summary.input_tokens} / out {summary.output_tokens} / cr {summary.cache_read_tokens}
        / cc {summary.cache_creation_tokens} · series in {seriesSum("input_tokens")} / out {seriesSum(
          "output_tokens"
        )} / cr {seriesSum("cache_read_tokens")} / cc {seriesSum("cache_creation_tokens")} · split 5m
        {summary.cache_creation_5m_tokens ?? "null"} / 1h {summary.cache_creation_1h_tokens ??
          "null"}
        · hit {overallHitRate === null ? "null" : (overallHitRate * 100).toFixed(4) + "%"}
      </p>
    {/if}
  {/if}
</div>

<style>
  .tokens-view {
    max-width: 60rem;
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

  .error {
    color: #b42318;
    line-height: 1.4;
  }

  .totals {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.45rem;
    margin-top: 0.9rem;
  }

  .total-tokens {
    font-size: 1.65rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .hit-rate-total {
    font-variant-numeric: tabular-nums;
  }

  .divider {
    opacity: 0.6;
  }

  .stale {
    opacity: 0.6;
    transition: opacity 0.15s ease 0.15s;
  }

  .chart-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
    gap: 1rem;
    margin-top: 1rem;
  }

  .chart-card {
    padding: 1rem 1.1rem;
    border: 1px solid rgba(0, 0, 0, 0.1);
    border-radius: 10px;
    background: #ffffff;
  }

  .chart-card :global(svg) {
    height: 130px;
  }

  .hit-rate-card {
    margin-top: 1rem;
  }

  .hit-rate-card :global(svg) {
    height: 150px;
  }

  h2 {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
    margin: 0 0 0.6rem;
    font-size: 0.85rem;
    font-weight: 650;
  }

  .card-total {
    margin-left: auto;
    font-weight: 500;
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
  }

  .swatch {
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 3px;
    flex-shrink: 0;
    align-self: center;
  }

  .footnote .swatch {
    display: inline-block;
    vertical-align: -0.05rem;
  }

  .footnote .swatch.one-h {
    margin-left: 0.5rem;
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
    .footnote {
      color: #9b9b9f;
    }

    .error {
      color: #ffa198;
    }

    .chart-card {
      background: #1f1f21;
      border-color: rgba(255, 255, 255, 0.12);
    }

    .empty {
      border-color: rgba(255, 255, 255, 0.22);
    }
  }
</style>
