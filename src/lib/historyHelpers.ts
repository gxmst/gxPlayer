import type { CatalogTrack, HistoryEntry, LibraryTrack } from "../types";

/**
 * Convert a history entry to a minimal LibraryTrack for playback.
 */
export function historyEntryToLibraryTrack(entry: HistoryEntry): LibraryTrack {
  return {
    id: -1,
    path: entry.path || "",
    title: entry.title,
    artist: entry.artist,
    album: "",
    durationSeconds: null,
    favorite: false,
    addedAtMs: 0,
  };
}

/**
 * Convert a history entry to a CatalogTrack for playback.
 */
export function historyEntryToCatalogTrack(entry: HistoryEntry): CatalogTrack {
  return {
    providerId: entry.providerId || "",
    providerTrackId: entry.providerTrackId || "",
    title: entry.title,
    artist: entry.artist,
    album: "",
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}
