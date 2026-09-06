import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauriClient";
import type { CacheEntryPage, CacheKey, CacheStatus, CatalogTrack, LibraryPlaylistItem, ViewId } from "../types";

export const CACHE_PAGE_SIZE = 100;
type LoadState = "loading" | "ready" | "error";

export function cacheIdentityKey(entry: CacheKey): string {
  return `${entry.providerId}\u0000${entry.providerTrackId}\u0000${entry.quality}`;
}

export function useCacheLibrary(view: ViewId, playlistItems: LibraryPlaylistItem[], hasOnlineTrack = false) {
  const summaryEnabled = hasOnlineTrack || ["settings", "library", "favorites", "now-playing", "playlist"].includes(view);
  const [cacheStatus, setCacheStatus] = useState<CacheStatus | null>(null);
  const [onlineFavorites, setOnlineFavorites] = useState<CatalogTrack[]>([]);
  const [cacheLoadState, setCacheLoadState] = useState<LoadState>("loading");
  const [cachePage, setCachePage] = useState<CacheEntryPage>({ entries: [], totalCount: 0, offset: 0, qualities: [] });
  const [cachePageLoadState, setCachePageLoadState] = useState<LoadState>("loading");
  const [availableCacheKeys, setAvailableCacheKeys] = useState<Set<string>>(new Set());
  const [cacheAvailabilityState, setCacheAvailabilityState] = useState<LoadState>("loading");
  const summaryGeneration = useRef(0);
  const pageGeneration = useRef(0);
  const availabilityGeneration = useRef(0);
  const requestedOffset = useRef(0);
  const current = useRef({ view, playlistItems });
  current.current = { view, playlistItems };

  const refreshSummary = useCallback(async () => {
    const generation = ++summaryGeneration.current;
    setCacheLoadState((state) => state === "ready" ? state : "loading");
    try {
      const [status, favorites] = await Promise.all([
        invoke<CacheStatus>("cache_status"),
        invoke<CatalogTrack[]>("cache_online_favorites"),
      ]);
      if (!status || typeof status !== "object" || !Array.isArray(favorites)) throw new Error("缓存与收藏返回了无效数据");
      if (generation !== summaryGeneration.current) return;
      setCacheStatus(status);
      setOnlineFavorites(favorites);
      setCacheLoadState("ready");
    } catch (error) {
      if (generation === summaryGeneration.current) setCacheLoadState("error");
      throw error;
    }
  }, []);

  const loadCachePage = useCallback(async (offset: number) => {
    const generation = ++pageGeneration.current;
    requestedOffset.current = offset;
    setCachePageLoadState("loading");
    try {
      const page = await invoke<CacheEntryPage>("cache_list_page", { offset, limit: CACHE_PAGE_SIZE });
      if (!page || !Array.isArray(page.entries) || !Array.isArray(page.qualities)
        || !Number.isSafeInteger(page.totalCount) || page.totalCount < 0
        || !Number.isSafeInteger(page.offset) || page.offset < 0 || page.entries.length > CACHE_PAGE_SIZE) {
        throw new Error("缓存列表返回了无效数据");
      }
      if (generation !== pageGeneration.current) return;
      requestedOffset.current = page.offset;
      setCachePage(page);
      setCachePageLoadState("ready");
    } catch (error) {
      if (generation === pageGeneration.current) setCachePageLoadState("error");
      throw error;
    }
  }, []);

  const refreshAvailability = useCallback(async () => {
    const generation = ++availabilityGeneration.current;
    const keys = current.current.playlistItems.filter((item) => item.kind === "cached");
    setCacheAvailabilityState("loading");
    try {
      const available: CacheKey[] = [];
      // Large legacy playlists can exceed one IPC batch.
      for (let offset = 0; offset < keys.length; offset += CACHE_PAGE_SIZE) {
        const batch = keys.slice(offset, offset + CACHE_PAGE_SIZE).map(({ providerId, providerTrackId, quality }) => ({ providerId, providerTrackId, quality }));
        const result = await invoke<CacheKey[]>("cache_available_keys", { keys: batch });
        if (generation !== availabilityGeneration.current) return;
        if (!Array.isArray(result)) throw new Error("缓存检查返回了无效数据");
        available.push(...result);
      }
      if (generation !== availabilityGeneration.current) return;
      setAvailableCacheKeys(new Set(available.map(cacheIdentityKey)));
      setCacheAvailabilityState("ready");
    } catch (error) {
      if (generation === availabilityGeneration.current) setCacheAvailabilityState("error");
      throw error;
    }
  }, []);

  const refreshCache = useCallback(async () => {
    await Promise.all([
      refreshSummary(),
      current.current.view === "library" ? loadCachePage(requestedOffset.current) : undefined,
      current.current.view === "playlist" ? refreshAvailability() : undefined,
    ]);
  }, [loadCachePage, refreshAvailability, refreshSummary]);

  useEffect(() => {
    if (!summaryEnabled) return;
    void refreshSummary().catch(() => undefined);
    if (view === "library") void loadCachePage(requestedOffset.current).catch(() => undefined);
    let disposed = false;
    let timer: number | undefined;
    const unlisten = listen<number>("gx-cache-changed", () => {
      if (disposed) return;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => { void refreshCache().catch(() => undefined); }, 120);
    }).catch(() => () => undefined);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      summaryGeneration.current += 1;
      pageGeneration.current += 1;
      availabilityGeneration.current += 1;
      void unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, [loadCachePage, refreshCache, refreshSummary, summaryEnabled, view]);

  useEffect(() => {
    if (view === "playlist") void refreshAvailability().catch(() => undefined);
  }, [playlistItems, refreshAvailability, view]);

  return {
    cacheStatus, setCacheStatus, onlineFavorites, cacheLoadState,
    cachePage, cachePageLoadState, loadCachePage, refreshCache,
    availableCacheKeys, cacheAvailabilityState,
  };
}
