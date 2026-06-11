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

/** Directory path cleaned for display: the home-dir prefix becomes `~`
 * (PRD FR-3, e.g. "~/Projects/luxurypresence/websites"). Unknown home or
 * a path outside it passes through unchanged. */
export function cleanPath(path: string, home: string | null | undefined): string {
  if (!home) return path;
  const root = home.endsWith("/") ? home.slice(0, -1) : home;
  if (root === "" || root === "/") return path;
  if (path === root) return "~";
  return path.startsWith(`${root}/`) ? `~${path.slice(root.length)}` : path;
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

/** Local date + time, e.g. "Jun 10, 14:26" (session start column). */
export function formatDateTime(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Local time of day with seconds, e.g. "14:26:44" (request timeline). */
export function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** Compact elapsed time: "<1s", "45s", "5m 12s", "2h 14m". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return "<1s";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}
