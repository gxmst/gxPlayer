import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CatalogTrack } from "../types";

export type AsyncSearchState = "idle" | "loading" | "ready" | "empty" | "error";

type SearchBucket = {
  state: AsyncSearchState;
  error: string | null;
};

const IDLE_BUCKET: SearchBucket = { state: "idle", error: null };

/**
 * Owns search request generations so a slow Tauri command can never overwrite a
 * newer query. Suggestions and full results intentionally have separate state.
 */
export function useCatalogSearch(query: string) {
  const [suggestions, setSuggestions] = useState<CatalogTrack[]>([]);
  const [suggestionBucket, setSuggestionBucket] = useState<SearchBucket>(IDLE_BUCKET);
  const [suggestionRetry, setSuggestionRetry] = useState(0);
  const suggestionGeneration = useRef(0);

  const [results, setResults] = useState<CatalogTrack[]>([]);
  const [resultsQuery, setResultsQuery] = useState("");
  const [resultsBucket, setResultsBucket] = useState<SearchBucket>(IDLE_BUCKET);
  const resultsGeneration = useRef(0);

  useEffect(() => {
    const normalized = query.trim();
    const generation = ++suggestionGeneration.current;
    if (!normalized) {
      setSuggestions([]);
      setSuggestionBucket(IDLE_BUCKET);
      return;
    }

    setSuggestionBucket({ state: "loading", error: null });
    const timer = window.setTimeout(async () => {
      try {
        const tracks = await invoke<CatalogTrack[]>("metadata_search", {
          query: normalized,
          limit: 9,
        });
        if (generation !== suggestionGeneration.current) return;
        setSuggestions(tracks);
        setSuggestionBucket({ state: tracks.length ? "ready" : "empty", error: null });
      } catch (error) {
        if (generation !== suggestionGeneration.current) return;
        setSuggestions([]);
        setSuggestionBucket({ state: "error", error: String(error) });
      }
    }, 200);

    return () => window.clearTimeout(timer);
  }, [query, suggestionRetry]);

  const search = useCallback(async (rawQuery: string): Promise<CatalogTrack[] | null> => {
    const normalized = rawQuery.trim();
    if (!normalized) return null;
    const generation = ++resultsGeneration.current;
    setResultsQuery(normalized);
    setResultsBucket({ state: "loading", error: null });
    try {
      const tracks = await invoke<CatalogTrack[]>("metadata_search", {
        query: normalized,
        limit: 40,
      });
      if (generation !== resultsGeneration.current) return null;
      setResults(tracks);
      setResultsBucket({ state: tracks.length ? "ready" : "empty", error: null });
      return tracks;
    } catch (error) {
      if (generation !== resultsGeneration.current) return null;
      setResults([]);
      setResultsBucket({ state: "error", error: String(error) });
      return null;
    }
  }, []);

  const retrySuggestions = useCallback(() => setSuggestionRetry((value) => value + 1), []);
  const retryResults = useCallback(() => {
    if (resultsQuery) void search(resultsQuery);
  }, [resultsQuery, search]);
  const seedResults = useCallback((tracks: CatalogTrack[], label: string) => {
    resultsGeneration.current += 1;
    setResultsQuery(label);
    setResults(tracks);
    setResultsBucket({ state: tracks.length ? "ready" : "empty", error: null });
  }, []);

  return {
    suggestions,
    suggestionState: suggestionBucket.state,
    suggestionError: suggestionBucket.error,
    retrySuggestions,
    results,
    resultsQuery,
    resultsState: resultsBucket.state,
    resultsError: resultsBucket.error,
    search,
    retryResults,
    seedResults,
  };
}
