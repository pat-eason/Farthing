<script module lang="ts">
  // Stacked bar chart over per-day buckets (task 5.3; the tokens view in
  // 5.5 reuses it). Pure SVG like the popover sparkline: buckets arrive
  // pre-aggregated (the ungrouped series provides explicit zero buckets for
  // gap days), so this component only scales and stacks - it never infers
  // values. Sparse data degrades gracefully: zero buckets draw no bar
  // (baseline only) but keep a full-height hover target for the tooltip,
  // and an all-zero series renders as a flat baseline.
  export interface ChartSegment {
    /** Stable identity (keyed rendering / legend correlation). */
    id: string;
    /** Tooltip line label. */
    label: string;
    color: string;
    value: number;
  }

  export interface ChartBucket {
    start_ms: number;
    /** Tooltip heading, e.g. "Wed, Jun 10". */
    label: string;
    /** Stack segments, bottom-up; one segment = a plain bar chart. */
    segments: ChartSegment[];
  }
</script>

<script lang="ts">
  import { formatDate } from "$lib/format";

  interface Props {
    buckets: ChartBucket[];
    formatValue: (value: number) => string;
    ariaLabel: string;
  }

  let { buckets, formatValue, ariaLabel }: Props = $props();

  /** ViewBox height; bars scale into it, the box stretches to the CSS size. */
  const HEIGHT = 100;
  /** Reserved headroom so the tallest bar doesn't touch the top edge. */
  const TOP_PAD = 4;
  const BASELINE = 1;

  const bucketTotal = (bucket: ChartBucket) =>
    bucket.segments.reduce((sum, segment) => sum + segment.value, 0);

  const maxTotal = $derived(buckets.reduce((max, bucket) => Math.max(max, bucketTotal(bucket)), 0));

  function scaled(value: number): number {
    if (maxTotal <= 0 || value <= 0) return 0;
    return (value / maxTotal) * (HEIGHT - TOP_PAD - BASELINE);
  }

  /** Stack offsets: y of each segment's top edge, bottom-up in order. */
  function stackTops(bucket: ChartBucket): number[] {
    let bottom = HEIGHT - BASELINE;
    return bucket.segments.map((segment) => {
      bottom -= scaled(segment.value);
      return bottom;
    });
  }

  function tooltip(bucket: ChartBucket): string {
    const total = `${bucket.label} · ${formatValue(bucketTotal(bucket))}`;
    if (bucket.segments.length <= 1) return total;
    const lines = bucket.segments
      .filter((segment) => segment.value > 0)
      .reverse() // top-of-stack first, matching the visual order
      .map((segment) => `${segment.label}: ${formatValue(segment.value)}`);
    return [total, ...lines].join("\n");
  }
</script>

<figure class="chart">
  {#if maxTotal > 0}
    <figcaption class="peak">peak day {formatValue(maxTotal)}</figcaption>
  {/if}
  <svg
    viewBox="0 0 {Math.max(buckets.length, 1)} {HEIGHT}"
    preserveAspectRatio="none"
    role="img"
    aria-label={ariaLabel}
  >
    <!-- Baseline spanning the full width, present even with no data. -->
    <rect
      class="baseline"
      x="0"
      y={HEIGHT - BASELINE}
      width={Math.max(buckets.length, 1)}
      height={BASELINE}
    />
    {#each buckets as bucket, i (bucket.start_ms)}
      {@const tops = stackTops(bucket)}
      <g>
        <title>{tooltip(bucket)}</title>
        <!-- Invisible full-height hover target so zero buckets still show
             their tooltip. -->
        <rect class="hover-target" x={i} y="0" width="1" height={HEIGHT} />
        {#each bucket.segments as segment, s (segment.id)}
          {#if segment.value > 0}
            <rect
              x={i + 0.08}
              y={tops[s]}
              width="0.84"
              height={scaled(segment.value)}
              fill={segment.color}
            />
          {/if}
        {/each}
      </g>
    {/each}
  </svg>
  {#if buckets.length > 0}
    <div class="axis">
      <span>{formatDate(buckets[0].start_ms)}</span>
      <span>{formatDate(buckets[buckets.length - 1].start_ms)}</span>
    </div>
  {/if}
</figure>

<style>
  .chart {
    margin: 0;
  }

  svg {
    display: block;
    width: 100%;
    height: 220px;
  }

  .peak {
    margin: 0 0 0.2rem;
    text-align: right;
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
    color: #6b6b6b;
  }

  .axis {
    display: flex;
    justify-content: space-between;
    margin-top: 0.25rem;
    font-size: 0.7rem;
    color: #6b6b6b;
  }

  .baseline {
    fill: rgba(0, 0, 0, 0.18);
  }

  .hover-target {
    fill: transparent;
  }

  g:hover .hover-target {
    fill: rgba(10, 132, 255, 0.08);
  }

  @media (prefers-color-scheme: dark) {
    .baseline {
      fill: rgba(255, 255, 255, 0.22);
    }

    .peak,
    .axis {
      color: #9b9b9f;
    }
  }
</style>
