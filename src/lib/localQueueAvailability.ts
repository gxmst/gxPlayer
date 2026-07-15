import type { PersistablePlaylistEntry } from "./playlistPersistence";

export type LocalPathAvailability = {
  path: string;
  available: boolean;
};

export function localQueuePaths(entries: readonly PersistablePlaylistEntry[]): string[] {
  return [...new Set(
    entries
      .filter((entry): entry is Extract<PersistablePlaylistEntry, { kind: "local" }> => entry.kind === "local")
      .map((entry) => entry.path),
  )];
}

export function unavailablePathsFromChecks(
  entries: readonly PersistablePlaylistEntry[],
  checks: readonly LocalPathAvailability[],
): Set<string> {
  const paths = localQueuePaths(entries);
  const expected = new Set(paths);
  const checked = new Map<string, boolean>();
  for (const check of checks) {
    if (expected.has(check.path)) checked.set(check.path, check.available);
  }
  if (checked.size !== expected.size) {
    throw new Error("本地路径检查结果不完整");
  }
  return new Set(paths.filter((path) => checked.get(path) === false));
}

export function relinkLocalQueuePath(
  entries: readonly PersistablePlaylistEntry[],
  oldPath: string,
  replacement: Extract<PersistablePlaylistEntry, { kind: "local" }>,
): PersistablePlaylistEntry[] {
  return entries.map((entry) => (
    entry.kind === "local" && entry.path === oldPath ? replacement : entry
  ));
}

export function engineMatchesLocalQueue(
  entries: readonly PersistablePlaylistEntry[],
  engineQueue: readonly { location: string; online: boolean }[],
): boolean {
  if (entries.length === 0 || entries.length !== engineQueue.length) return false;
  return entries.every((entry, index) => (
    entry.kind === "local"
    && engineQueue[index]?.online === false
    && engineQueue[index]?.location === entry.path
  ));
}
