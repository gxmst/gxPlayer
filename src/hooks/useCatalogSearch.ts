import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CatalogTrack } from "../types";

export type AsyncSearchState = "idle" | "loading" | "ready" | "empty" | "error";

type SearchBucket = {
  state: AsyncSearchState;
  error: string | null;
};

type CatalogSearchBatch = {
  requestId: string;
  providerId: string;
  tracks: CatalogTrack[];
  error: string | null;
};

type SearchLane = "searchSuggestions" | "searchResults";

const IDLE_BUCKET: SearchBucket = { state: "idle", error: null };
let requestSequence = 0;

function nextRequestId(kind: "suggestion" | "results"): string {
  requestSequence += 1;
  return `${kind}-${Date.now()}-${requestSequence}`;
}

function cancelMetadataRequest(lane: SearchLane, requestId: string): void {
  void invoke("metadata_cancel_request", { lane, requestId }).catch(() => undefined);
}

function catalogKey(track: CatalogTrack): string {
  return `${track.providerId}:${track.providerTrackId}`;
}

function mergeTracks(current: CatalogTrack[], incoming: CatalogTrack[]): CatalogTrack[] {
  if (!incoming.length) return current;
  const merged = new Map(current.map((track) => [catalogKey(track), track]));
  incoming.forEach((track) => merged.set(catalogKey(track), track));
  return [...merged.values()];
}

/**
 * Owns search request generations so a slow Tauri command or event can never
 * overwrite a newer query. Suggestions and full results use separate streams.
 */
export function useCatalogSearch(query: string) {
  const [suggestions, setSuggestions] = useState<CatalogTrack[]>([]);
  const [suggestionBucket, setSuggestionBucket] = useState<SearchBucket>(IDLE_BUCKET);
  const [suggestionRetry, setSuggestionRetry] = useState(0);
  const suggestionGeneration = useRef(0);
  const activeSuggestionRequest = useRef<string | null>(null);

  const [results, setResults] = useState<CatalogTrack[]>([]);
  const [resultsQuery, setResultsQuery] = useState("");
  const [resultsBucket, setResultsBucket] = useState<SearchBucket>(IDLE_BUCKET);
  const resultsGeneration = useRef(0);
  const activeResultsRequest = useRef<string | null>(null);
  const resultsUnlisten = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    const normalized = query.trim();
    const generation = ++suggestionGeneration.current;
    if (!normalized) {
      setSuggestions([]);
      setSuggestionBucket(IDLE_BUCKET);
      return;
    }

    setSuggestions([]);
    setSuggestionBucket({ state: "loading", error: null });
    const requestId = nextRequestId("suggestion");
    activeSuggestionRequest.current = requestId;
    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    const timer = window.setTimeout(() => {
      void (async () => {
        try {
          const stop = await listen<CatalogSearchBatch>("gx-catalog-search-batch", (event) => {
            if (
              disposed
              || generation !== suggestionGeneration.current
              || event.payload.requestId !== requestId
            ) return;
            setSuggestions((current) => mergeTracks(current, event.payload.tracks));
          });
          if (disposed || generation !== suggestionGeneration.current) {
            stop();
            return;
          }
          unlisten = stop;
          const tracks = await invoke<CatalogTrack[]>("metadata_search", {
            query: normalized,
            limit: 9,
            requestId,
            lane: "searchSuggestions",
          });
          if (disposed || generation !== suggestionGeneration.current) return;
          setSuggestions(tracks);
          setSuggestionBucket({ state: tracks.length ? "ready" : "empty", error: null });
        } catch (error) {
          if (disposed || generation !== suggestionGeneration.current) return;
          setSuggestions([]);
          setSuggestionBucket({ state: "error", error: String(error) });
        } finally {
          unlisten?.();
          unlisten = null;
          if (activeSuggestionRequest.current === requestId) {
            activeSuggestionRequest.current = null;
          }
        }
      })();
    }, 200);

    return () => {
      disposed = true;
      window.clearTimeout(timer);
      unlisten?.();
      unlisten = null;
      if (activeSuggestionRequest.current === requestId) {
        activeSuggestionRequest.current = null;
        cancelMetadataRequest("searchSuggestions", requestId);
      }
    };
  }, [query, suggestionRetry]);

  const search = useCallback(async (rawQuery: string): Promise<CatalogTrack[] | null> => {
    const normalized = rawQuery.trim();
    if (!normalized) return null;
    const generation = ++resultsGeneration.current;
    const previousRequestId = activeResultsRequest.current;
    activeResultsRequest.current = null;
    if (previousRequestId) cancelMetadataRequest("searchResults", previousRequestId);
    const requestId = nextRequestId("results");
    activeResultsRequest.current = requestId;
    resultsUnlisten.current?.();
    resultsUnlisten.current = null;
    setResultsQuery(normalized);
    setResults([]);
    setResultsBucket({ state: "loading", error: null });
    let stopListener: UnlistenFn | null = null;
    try {
      const stop = await listen<CatalogSearchBatch>("gx-catalog-search-batch", (event) => {
        if (
          generation !== resultsGeneration.current
          || event.payload.requestId !== requestId
        ) return;
        setResults((current) => mergeTracks(current, event.payload.tracks));
      });
      if (generation !== resultsGeneration.current) {
        stop();
        return null;
      }
      stopListener = stop;
      resultsUnlisten.current = stop;
      const tracks = await invoke<CatalogTrack[]>("metadata_search", {
        query: normalized,
        limit: 40,
        requestId,
        lane: "searchResults",
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
    } finally {
      stopListener?.();
      if (resultsUnlisten.current === stopListener) resultsUnlisten.current = null;
      if (activeResultsRequest.current === requestId) {
        activeResultsRequest.current = null;
      }
    }
  }, []);

  useEffect(() => () => {
    suggestionGeneration.current += 1;
    resultsGeneration.current += 1;
    const suggestionRequestId = activeSuggestionRequest.current;
    activeSuggestionRequest.current = null;
    if (suggestionRequestId) cancelMetadataRequest("searchSuggestions", suggestionRequestId);
    const resultsRequestId = activeResultsRequest.current;
    activeResultsRequest.current = null;
    if (resultsRequestId) cancelMetadataRequest("searchResults", resultsRequestId);
    resultsUnlisten.current?.();
    resultsUnlisten.current = null;
  }, []);

  const retrySuggestions = useCallback(() => setSuggestionRetry((value) => value + 1), []);
  const retryResults = useCallback(() => {
    if (resultsQuery) void search(resultsQuery);
  }, [resultsQuery, search]);
  const seedResults = useCallback((tracks: CatalogTrack[], label: string) => {
    resultsGeneration.current += 1;
    const requestId = activeResultsRequest.current;
    activeResultsRequest.current = null;
    if (requestId) cancelMetadataRequest("searchResults", requestId);
    resultsUnlisten.current?.();
    resultsUnlisten.current = null;
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
