import type { HistoryEntry } from "../types";

export type GroupedHistoryEntry = {
  entry: HistoryEntry;
  count: number;
};

function normalizedText(value: string): string {
  return value.trim().replace(/\s+/g, " ").toLocaleLowerCase();
}

function normalizedWindowsPath(path: string): string {
  return path
    .trim()
    .replace(/[\\/]+/g, "\\")
    .replace(/\\$/, "")
    .toLocaleLowerCase("en-US");
}

export function historyEntryIdentity(entry: HistoryEntry): string {
  if (entry.kind === "local" && entry.path?.trim()) {
    return JSON.stringify(["local", normalizedWindowsPath(entry.path)]);
  }

  const providerId = entry.providerId?.trim();
  const providerTrackId = entry.providerTrackId?.trim();
  if (providerId && providerTrackId) {
    return JSON.stringify(["provider", providerId, providerTrackId]);
  }

  if (entry.path?.trim()) {
    return JSON.stringify(["local", normalizedWindowsPath(entry.path)]);
  }

  return JSON.stringify(["metadata", normalizedText(entry.title), normalizedText(entry.artist)]);
}

export function groupConsecutiveHistory(entries: readonly HistoryEntry[]): GroupedHistoryEntry[] {
  const groups: GroupedHistoryEntry[] = [];
  let previousIdentity: string | null = null;

  for (const entry of entries) {
    const identity = historyEntryIdentity(entry);
    const previous = groups[groups.length - 1];
    if (previous && identity === previousIdentity) {
      previous.count += 1;
    } else {
      groups.push({ entry, count: 1 });
      previousIdentity = identity;
    }
  }

  return groups;
}
