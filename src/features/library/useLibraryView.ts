import { useMemo, useState } from "react";
import type { LibraryTrack } from "../../types";

export type LibrarySort = "added" | "title" | "artist" | "album";
export type LibraryScope = "all" | "recent" | "artists" | "albums" | "missing";

function matchesTrack(track: LibraryTrack, query: string, includePath = false): boolean {
  const haystack = includePath
    ? `${track.title}\n${track.artist}\n${track.album}\n${track.path}`
    : `${track.title}\n${track.artist}\n${track.album}`;
  return haystack.toLocaleLowerCase().includes(query);
}

function facetCounts(tracks: LibraryTrack[], field: "artist" | "album", fallback: string) {
  const counts = new Map<string, number>();
  tracks.forEach((track) => {
    const value = track[field] || fallback;
    counts.set(value, (counts.get(value) ?? 0) + 1);
  });
  return [...counts.entries()].sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0], "zh-CN"));
}

export function useLibraryView(library: LibraryTrack[], searchQuery: string, resultsQuery: string) {
  const [libraryQuery, setLibraryQuery] = useState("");
  const [librarySort, setLibrarySort] = useState<LibrarySort>("added");
  const [libraryScope, setLibraryScope] = useState<LibraryScope>("all");
  const [selectedLibraryIds, setSelectedLibraryIds] = useState<number[]>([]);

  const filteredLibrary = useMemo(() => {
    const query = libraryQuery.trim().toLocaleLowerCase();
    const recentCutoff = Date.now() - 30 * 86_400_000;
    const scoped = library.filter((track) => {
      if (libraryScope === "recent" && track.addedAtMs < recentCutoff) return false;
      if (libraryScope === "missing" && !track.missing) return false;
      return !query || matchesTrack(track, query, true);
    });
    return [...scoped].sort((left, right) => {
      if (librarySort === "added") return right.addedAtMs - left.addedAtMs;
      const leftValue = left[librarySort] || "";
      const rightValue = right[librarySort] || "";
      return leftValue.localeCompare(rightValue, "zh-CN", { sensitivity: "base" });
    });
  }, [library, libraryQuery, libraryScope, librarySort]);

  const libraryArtists = useMemo(() => facetCounts(library, "artist", "未知歌手"), [library]);
  const libraryAlbums = useMemo(() => facetCounts(library, "album", "未知专辑"), [library]);
  const localSuggestions = useMemo(() => {
    const query = searchQuery.trim().toLocaleLowerCase();
    return query ? library.filter((track) => matchesTrack(track, query)).slice(0, 4) : [];
  }, [library, searchQuery]);
  const localSearchResults = useMemo(() => {
    const query = resultsQuery.trim().toLocaleLowerCase();
    return query ? library.filter((track) => matchesTrack(track, query)) : [];
  }, [library, resultsQuery]);

  return {
    libraryQuery,
    setLibraryQuery,
    librarySort,
    setLibrarySort,
    libraryScope,
    setLibraryScope,
    selectedLibraryIds,
    setSelectedLibraryIds,
    filteredLibrary,
    libraryArtists,
    libraryAlbums,
    localSuggestions,
    localSearchResults,
  };
}
