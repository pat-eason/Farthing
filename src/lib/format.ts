// Shared display formatting for the popover and the analysis views
// (tasks 4.2, 5.3-5.6): one set of rules so cost/token values and project
// names read identically everywhere.

/** Compact cost display: "$0.00", "<$0.01", "$4.20", "$123", "$12,345". */
export function formatCost(value: number): string {
  if (value === 0) return "$0.00";
  if (value < 0.01) return "<$0.01";
  if (value >= 1000) return `$${Math.round(value).toLocaleString()}`;
  if (value >= 100) return `$${value.toFixed(0)}`;
  return `$${value.toFixed(2)}`;
}

/** Compact token-count display: "950", "1.2k", "3.4M", "1.1B". */
export function formatTokens(value: number): string {
  if (value < 1_000) return String(value);
  if (value < 1_000_000) return `${(value / 1_000).toFixed(1)}k`;
  if (value < 1_000_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  return `${(value / 1_000_000_000).toFixed(1)}B`;
}

/** Display name for a project: the last path segment of the session cwd. */
export function projectName(cwd: string | null): string {
  if (cwd === null) return "(unknown project)";
  const segments = cwd.split("/").filter((s) => s.length > 0);
  return segments[segments.length - 1] ?? cwd;
}

/** Short local date, e.g. "Wed, Jun 10". */
export function formatDay(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
  });
}

/** Local date with year, e.g. "Jun 10, 2026" (chart axis edges). */
export function formatDate(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
