import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../../lib/tauriClient";
import type { LibraryTrack, PlaylistSummary, ViewId } from "../../types";

export function useLibraryData(view: ViewId) {
  const [library, setLibrary] = useState<LibraryTrack[]>([]);
  const [favorites, setFavorites] = useState<LibraryTrack[]>([]);
  const [playlists, setPlaylists] = useState<PlaylistSummary[]>([]);
  const [libraryLoadState, setLibraryLoadState] = useState<"loading" | "ready" | "error">("loading");
  const readGeneration = useRef(0);
  const scanGeneration = useRef(0);
  const missingByPath = useRef(new Map<string, boolean>());

  const refreshLibrary = useCallback(async (scanMissing = false): Promise<LibraryTrack[]> => {
    const generation = ++readGeneration.current;
    // A mutation followed by a refresh invalidates an older filesystem snapshot.
    scanGeneration.current += 1;
    try {
      const [tracks, favoriteTracks, nextPlaylists] = await Promise.all([
        invoke<LibraryTrack[]>(scanMissing ? "library_scan_missing" : "library_tracks"),
        invoke<LibraryTrack[]>("library_favorites"),
        invoke<PlaylistSummary[]>("library_playlists"),
      ]);
      if (!Array.isArray(tracks) || !Array.isArray(favoriteTracks) || !Array.isArray(nextPlaylists)) throw new Error("曲库返回了无效数据");
      if (generation !== readGeneration.current) return tracks;
      if (scanMissing) missingByPath.current = new Map(tracks.map((track) => [track.path, Boolean(track.missing)]));
      const applyMissing = (track: LibraryTrack) => ({ ...track, missing: missingByPath.current.get(track.path) ?? Boolean(track.missing) });
      const checked = tracks.map(applyMissing);
      setLibrary(checked);
      setFavorites(favoriteTracks.map(applyMissing));
      setPlaylists(nextPlaylists);
      setLibraryLoadState("ready");
      return checked;
    } catch (error) {
      if (generation === readGeneration.current) setLibraryLoadState((state) => state === "ready" ? state : "error");
      throw error;
    }
  }, []);

  useEffect(() => {
    void refreshLibrary().catch((error) => console.warn("[GXPlayer] initial library read failed", error));
    return () => { readGeneration.current += 1; scanGeneration.current += 1; };
  }, [refreshLibrary]);

  useEffect(() => {
    if (libraryLoadState !== "ready" || !["library", "favorites", "playlist"].includes(view)) return;
    const generation = ++scanGeneration.current;
    void invoke<LibraryTrack[]>("library_scan_missing").then((tracks) => {
      if (generation !== scanGeneration.current || !Array.isArray(tracks)) return;
      missingByPath.current = new Map(tracks.map((track) => [track.path, Boolean(track.missing)]));
      const applyMissing = (current: LibraryTrack[]) => current.map((track) => ({
        ...track, missing: missingByPath.current.get(track.path) ?? Boolean(track.missing),
      }));
      setLibrary(applyMissing);
      setFavorites(applyMissing);
    }).catch((error) => console.warn("[GXPlayer] library availability scan failed", error));
    return () => { scanGeneration.current += 1; };
  }, [libraryLoadState, view]);

  return { library, favorites, playlists, libraryLoadState, refreshLibrary };
}
