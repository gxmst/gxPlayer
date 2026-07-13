import type { CatalogTrack, PlayMode } from "../types";

export const PLAYLIST_SESSION_STORAGE_KEY = "gxplayer.playlistSession";

const PLAYLIST_SESSION_VERSION = 1;
const MAX_PLAYLIST_ENTRIES = 10_000;
const MAX_SERIALIZED_LENGTH = 5 * 1024 * 1024;
const PLAY_MODES: readonly PlayMode[] = ["sequential", "repeat_all", "repeat_one", "shuffle"];
const QUALITY_PREFERENCES = ["auto", "128k", "320k", "flac", "flac24bit"] as const;

export type QualityPreference = (typeof QUALITY_PREFERENCES)[number];

/** Matches App's logical queue. It never contains a resolved online media request. */
export type PersistablePlaylistEntry =
  | {
      kind: "local";
      path: string;
      title: string;
      artist: string;
      durationSeconds: number | null;
    }
  | {
      kind: "online";
      track: CatalogTrack;
      quality: QualityPreference;
    }
  | {
      kind: "cached";
      providerId: string;
      providerTrackId: string;
      quality: string;
      title: string;
      artist: string;
    };

export type PlaylistSessionState = {
  playlist: PersistablePlaylistEntry[];
  currentIndex: number | null;
  playMode: PlayMode;
};

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

type StoredPlaylistSession = PlaylistSessionState & {
  version: typeof PLAYLIST_SESSION_VERSION;
};

function emptySession(): PlaylistSessionState {
  return {
    playlist: [],
    currentIndex: null,
    playMode: "sequential",
  };
}

function defaultStorage(): StorageLike | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isNonEmptyString(value: unknown): value is string {
  return isString(value) && value.trim().length > 0;
}

function isNullableNonNegativeNumber(value: unknown): value is number | null {
  return value === null || (typeof value === "number" && Number.isFinite(value) && value >= 0);
}

function isPlayMode(value: unknown): value is PlayMode {
  return PLAY_MODES.some((mode) => mode === value);
}

function isQualityPreference(value: unknown): value is QualityPreference {
  return QUALITY_PREFERENCES.some((quality) => quality === value);
}

function cloneJsonValue(value: unknown): { ok: true; value: unknown } | { ok: false } {
  try {
    const encoded = JSON.stringify(value);
    if (encoded === undefined) return { ok: false };
    return { ok: true, value: JSON.parse(encoded) as unknown };
  } catch {
    return { ok: false };
  }
}

function normalizeCatalogTrack(value: unknown, requireSanitized: boolean): CatalogTrack | null {
  if (!isRecord(value)) return null;
  if (!isNonEmptyString(value.providerId) || !isNonEmptyString(value.providerTrackId)) return null;
  if (!isNonEmptyString(value.title) || !isString(value.artist) || !isString(value.album)) return null;
  if (!isNullableNonNegativeNumber(value.durationMs)) return null;
  if (value.artworkUrl !== null && !isString(value.artworkUrl)) return null;
  if (requireSanitized && value.preview !== null) return null;

  const resolverPayload = cloneJsonValue(value.resolverPayload);
  if (!resolverPayload.ok) return null;

  return {
    providerId: value.providerId,
    providerTrackId: value.providerTrackId,
    title: value.title,
    artist: value.artist,
    album: value.album,
    durationMs: value.durationMs,
    artworkUrl: value.artworkUrl,
    resolverPayload: resolverPayload.value,
    // Catalog previews contain a direct media request. They must be resolved again after restart.
    preview: null,
  };
}

function normalizeEntry(value: unknown, requireSanitized: boolean): PersistablePlaylistEntry | null {
  if (!isRecord(value)) return null;

  if (value.kind === "local") {
    if (!isNonEmptyString(value.path) || !isNonEmptyString(value.title) || !isString(value.artist)) {
      return null;
    }
    if (!isNullableNonNegativeNumber(value.durationSeconds)) return null;
    return {
      kind: "local",
      path: value.path,
      title: value.title,
      artist: value.artist,
      durationSeconds: value.durationSeconds,
    };
  }

  if (value.kind === "online") {
    const track = normalizeCatalogTrack(value.track, requireSanitized);
    if (!track || !isQualityPreference(value.quality)) return null;
    return { kind: "online", track, quality: value.quality };
  }

  if (value.kind === "cached") {
    if (!isNonEmptyString(value.providerId) || !isNonEmptyString(value.providerTrackId)) return null;
    if (!isNonEmptyString(value.quality) || !isNonEmptyString(value.title) || !isString(value.artist)) {
      return null;
    }
    return {
      kind: "cached",
      providerId: value.providerId,
      providerTrackId: value.providerTrackId,
      quality: value.quality,
      title: value.title,
      artist: value.artist,
    };
  }

  return null;
}

function normalizeSession(value: unknown, requireSanitized: boolean): PlaylistSessionState | null {
  if (!isRecord(value) || !Array.isArray(value.playlist) || value.playlist.length > MAX_PLAYLIST_ENTRIES) {
    return null;
  }
  if (!isPlayMode(value.playMode)) return null;

  const playlist: PersistablePlaylistEntry[] = [];
  for (const rawEntry of value.playlist) {
    const entry = normalizeEntry(rawEntry, requireSanitized);
    if (!entry) return null;
    playlist.push(entry);
  }

  const currentIndex = value.currentIndex;
  if (currentIndex !== null) {
    if (!Number.isInteger(currentIndex) || typeof currentIndex !== "number") return null;
    if (currentIndex < 0 || currentIndex >= playlist.length) return null;
  }

  return { playlist, currentIndex, playMode: value.playMode };
}

function discardInvalidStorage(storage: StorageLike): void {
  try {
    storage.removeItem(PLAYLIST_SESSION_STORAGE_KEY);
  } catch {
    // Storage may be unavailable (privacy mode, quota implementation, or WebView shutdown).
  }
}

export function loadPlaylistSession(storage: StorageLike | null = defaultStorage()): PlaylistSessionState {
  if (!storage) return emptySession();

  let raw: string | null;
  try {
    raw = storage.getItem(PLAYLIST_SESSION_STORAGE_KEY);
  } catch {
    return emptySession();
  }
  if (raw === null) return emptySession();
  if (raw.length > MAX_SERIALIZED_LENGTH) {
    discardInvalidStorage(storage);
    return emptySession();
  }

  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed) || parsed.version !== PLAYLIST_SESSION_VERSION) {
      discardInvalidStorage(storage);
      return emptySession();
    }
    const session = normalizeSession(parsed, true);
    if (!session) {
      discardInvalidStorage(storage);
      return emptySession();
    }
    return session;
  } catch {
    discardInvalidStorage(storage);
    return emptySession();
  }
}

export function savePlaylistSession(
  state: PlaylistSessionState,
  storage: StorageLike | null = defaultStorage(),
): boolean {
  if (!storage) return false;
  const session = normalizeSession(state, false);
  if (!session) return false;

  const stored: StoredPlaylistSession = {
    version: PLAYLIST_SESSION_VERSION,
    ...session,
  };
  try {
    const serialized = JSON.stringify(stored);
    if (serialized.length > MAX_SERIALIZED_LENGTH) return false;
    storage.setItem(PLAYLIST_SESSION_STORAGE_KEY, serialized);
    return true;
  } catch {
    return false;
  }
}

export function clearPlaylistSession(storage: StorageLike | null = defaultStorage()): boolean {
  if (!storage) return false;
  try {
    storage.removeItem(PLAYLIST_SESSION_STORAGE_KEY);
    return true;
  } catch {
    return false;
  }
}

/** Remove local entries that no longer exist while keeping the selected song stable. */
export function filterUnavailableLocalEntries(
  state: PlaylistSessionState,
  availableLocalPaths: ReadonlySet<string>,
): PlaylistSessionState {
  const playlist: PersistablePlaylistEntry[] = [];
  let currentIndex: number | null = null;
  let keptBeforeCurrent = 0;

  state.playlist.forEach((entry, index) => {
    const keep = entry.kind !== "local" || availableLocalPaths.has(entry.path);
    if (!keep) return;

    if (state.currentIndex !== null && index < state.currentIndex) keptBeforeCurrent += 1;
    if (index === state.currentIndex) currentIndex = playlist.length;
    playlist.push(entry);
  });

  if (state.currentIndex !== null && currentIndex === null && playlist.length > 0) {
    currentIndex = Math.min(keptBeforeCurrent, playlist.length - 1);
  }

  return {
    playlist,
    currentIndex,
    playMode: state.playMode,
  };
}
