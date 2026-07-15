export type PlaybackStartOutcome = "started" | "failed" | "cancelled" | "stale";

export type PlaybackStartResult = {
  outcome: PlaybackStartOutcome;
  error?: unknown;
  failureKind?: "track_unavailable" | "no_source" | "network" | "authentication" | "rate_limited" | "unknown";
};

export const STARTED: PlaybackStartResult = { outcome: "started" };

export function shouldSkipAfterStart(result: PlaybackStartResult): boolean {
  return result.outcome === "failed" && result.failureKind === "track_unavailable";
}

export function nextOptionIndex(current: number, count: number, direction: 1 | -1): number {
  if (count <= 0) return -1;
  if (current < 0) return direction === 1 ? 0 : count - 1;
  return (current + direction + count) % count;
}

export function putLruValue<T>(record: Record<string, T>, key: string, value: T, limit: number): Record<string, T> {
  const next = { ...record };
  delete next[key];
  next[key] = value;
  const overflow = Object.keys(next).length - Math.max(1, limit);
  if (overflow > 0) {
    Object.keys(next).slice(0, overflow).forEach((oldest) => delete next[oldest]);
  }
  return next;
}
