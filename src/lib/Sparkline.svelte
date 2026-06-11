<script lang="ts">
  // Cost sparkline for the popover (task 4.3): one bar per local calendar
  // day, oldest left, today right (highlighted). Pure SVG; the series
  // arrives pre-bucketed from the `daily_costs` command with explicit zero
  // buckets for gap days, so this component only scales bars and never
  // infers values. Sparse data degrades gracefully: zero days draw no bar
  // (baseline only) but keep a full-height hover target for the tooltip,
  // and an all-zero series renders as a flat baseline.
  import type { DailyCost } from "$lib/metrics";

  let { series }: { series: DailyCost[] } = $props();

  /** ViewBox height; bars scale into it, the box stretches to the CSS size. */
  const HEIGHT = 40;
  /** Reserved headroom so the tallest bar doesn't touch the top edge. */
  const TOP_PAD = 2;
  const BASELINE = 1;

  const maxCost = $derived(series.reduce((max, day) => Math.max(max, day.cost_usd), 0));

  function barHeight(cost: number): number {
    if (maxCost <= 0 || cost <= 0) return 0;
    return (cost / maxCost) * (HEIGHT - TOP_PAD - BASELINE);
  }

  function tooltip(day: DailyCost): string {
    const date = new Date(day.day_start_ms).toLocaleDateString(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
    });
    const requests = `${day.requests} request${day.requests === 1 ? "" : "s"}`;
    return `${date} · $${day.cost_usd.toFixed(2)} · ${requests}`;
  }
</script>

<svg
  class="sparkline"
  viewBox="0 0 {Math.max(series.length, 1)} {HEIGHT}"
  preserveAspectRatio="none"
  role="img"
  aria-label="Daily cost, last {series.length} days"
>
  <!-- Baseline spanning the full width, present even with no data. -->
  <rect
    class="baseline"
    x="0"
    y={HEIGHT - BASELINE}
    width={Math.max(series.length, 1)}
    height={BASELINE}
  />
  {#each series as day, i (day.day_start_ms)}
    <g>
      <title>{tooltip(day)}</title>
      <!-- Invisible full-height hover target so zero-cost days still show
           their tooltip. -->
      <rect class="hover-target" x={i} y="0" width="1" height={HEIGHT} />
      {#if day.cost_usd > 0}
        <rect
          class="bar"
          class:today={i === series.length - 1}
          x={i + 0.12}
          y={HEIGHT - BASELINE - barHeight(day.cost_usd)}
          width="0.76"
          height={barHeight(day.cost_usd)}
        />
      {/if}
    </g>
  {/each}
</svg>

<style>
  .sparkline {
    display: block;
    width: 100%;
    height: 44px;
  }

  .baseline {
    fill: rgba(0, 0, 0, 0.18);
  }

  .hover-target {
    fill: transparent;
  }

  .bar {
    fill: #8e8e93;
  }

  .bar.today {
    fill: #0a84ff;
  }

  g:hover .bar {
    fill: #0a84ff;
  }

  g:hover .hover-target {
    fill: rgba(10, 132, 255, 0.08);
  }

  @media (prefers-color-scheme: dark) {
    .baseline {
      fill: rgba(255, 255, 255, 0.22);
    }

    .bar {
      fill: #98989d;
    }

    .bar.today {
      fill: #409cff;
    }

    g:hover .bar {
      fill: #409cff;
    }
  }
</style>
