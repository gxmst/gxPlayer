// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogTrack } from "../types";
import { useCatalogSearch } from "./useCatalogSearch";

const eventMock = vi.hoisted(() => ({
  handlers: new Set<(event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => {
    eventMock.handlers.add(handler);
    return () => eventMock.handlers.delete(handler);
  }),
}));

function track(title: string, providerId = "test"): CatalogTrack {
  return {
    providerId,
    providerTrackId: title,
    title,
    artist: "Artist",
    album: "Album",
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function emitBatch(payload: {
  requestId: string;
  providerId: string;
  tracks: CatalogTrack[];
  error?: string | null;
}) {
  eventMock.handlers.forEach((handler) => handler({
    payload: { error: null, ...payload },
  }));
}

function commandCalls(command: string) {
  return vi.mocked(invoke).mock.calls.filter(([calledCommand]) => calledCommand === command);
}

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
  eventMock.handlers.clear();
});

describe("useCatalogSearch", () => {
  it("keeps a slow older suggestion response from replacing the latest query", async () => {
    vi.useFakeTimers();
    const older = deferred<CatalogTrack[]>();
    const latest = deferred<CatalogTrack[]>();
    vi.mocked(invoke).mockImplementation((command, args) => {
      if (command === "metadata_cancel_request") return Promise.resolve(undefined) as never;
      const query = (args as { query: string }).query;
      return (query === "older" ? older.promise : latest.promise) as never;
    });

    const { result, rerender } = renderHook(
      ({ query }) => useCatalogSearch(query),
      { initialProps: { query: "older" } },
    );
    await act(async () => { vi.advanceTimersByTime(210); });

    rerender({ query: "latest" });
    const olderArgs = commandCalls("metadata_search")[0]?.[1] as {
      requestId: string;
      lane: string;
    };
    expect(olderArgs.lane).toBe("searchSuggestions");
    expect(invoke).toHaveBeenCalledWith("metadata_cancel_request", {
      lane: "searchSuggestions",
      requestId: olderArgs.requestId,
    });
    await act(async () => { vi.advanceTimersByTime(210); });
    await act(async () => { latest.resolve([track("latest result")]); });
    expect(result.current.suggestions[0]?.title).toBe("latest result");

    await act(async () => { older.resolve([track("older result")]); });
    expect(result.current.suggestions[0]?.title).toBe("latest result");
    expect(result.current.suggestionState).toBe("ready");
  });

  it("shows provider batches before the final search response completes", async () => {
    const pending = deferred<CatalogTrack[]>();
    vi.mocked(invoke).mockImplementation((command) => (
      command === "metadata_cancel_request"
        ? Promise.resolve(undefined) as never
        : pending.promise as never
    ));
    const { result } = renderHook(() => useCatalogSearch(""));

    let searchPromise: Promise<CatalogTrack[] | null> | undefined;
    await act(async () => {
      searchPromise = result.current.search("hello");
    });
    await waitFor(() => expect(commandCalls("metadata_search")).toHaveLength(1));
    const args = commandCalls("metadata_search")[0]?.[1] as { requestId: string; lane: string };
    expect(args.lane).toBe("searchResults");

    act(() => emitBatch({
      requestId: args.requestId,
      providerId: "fast",
      tracks: [track("fast result", "fast")],
    }));
    expect(result.current.results.map((item) => item.title)).toEqual(["fast result"]);
    expect(result.current.resultsState).toBe("loading");

    await act(async () => pending.resolve([
      track("fast result", "fast"),
      track("slow result", "slow"),
    ]));
    await searchPromise;
    expect(result.current.results.map((item) => item.title)).toEqual(["fast result", "slow result"]);
    expect(result.current.resultsState).toBe("ready");
  });

  it("ignores batches and final responses from an obsolete full search", async () => {
    const older = deferred<CatalogTrack[]>();
    const latest = deferred<CatalogTrack[]>();
    vi.mocked(invoke).mockImplementation((command, args) => {
      if (command === "metadata_cancel_request") return Promise.resolve(undefined) as never;
      const query = (args as { query: string }).query;
      return (query === "older" ? older.promise : latest.promise) as never;
    });
    const { result } = renderHook(() => useCatalogSearch(""));

    await act(async () => { void result.current.search("older"); });
    await waitFor(() => expect(commandCalls("metadata_search")).toHaveLength(1));
    const olderArgs = commandCalls("metadata_search")[0]?.[1] as { requestId: string };
    await act(async () => { void result.current.search("latest"); });
    await waitFor(() => expect(commandCalls("metadata_search")).toHaveLength(2));
    expect(invoke).toHaveBeenCalledWith("metadata_cancel_request", {
      lane: "searchResults",
      requestId: olderArgs.requestId,
    });

    act(() => emitBatch({
      requestId: olderArgs.requestId,
      providerId: "slow",
      tracks: [track("obsolete")],
    }));
    expect(result.current.results).toEqual([]);

    await act(async () => latest.resolve([track("latest result")]));
    await act(async () => older.resolve([track("older result")]));
    expect(result.current.results.map((item) => item.title)).toEqual(["latest result"]);
  });

  it("cancels active result requests when seeding results and unmounting", async () => {
    const pending = deferred<CatalogTrack[]>();
    vi.mocked(invoke).mockImplementation((command) => (
      command === "metadata_cancel_request"
        ? Promise.resolve(undefined) as never
        : pending.promise as never
    ));
    const { result, unmount } = renderHook(() => useCatalogSearch(""));

    await act(async () => { void result.current.search("first"); });
    await waitFor(() => expect(commandCalls("metadata_search")).toHaveLength(1));
    const firstArgs = commandCalls("metadata_search")[0]?.[1] as { requestId: string };
    act(() => result.current.seedResults([track("seeded")], "seed"));
    expect(invoke).toHaveBeenCalledWith("metadata_cancel_request", {
      lane: "searchResults",
      requestId: firstArgs.requestId,
    });

    await act(async () => { void result.current.search("second"); });
    await waitFor(() => expect(commandCalls("metadata_search")).toHaveLength(2));
    const secondArgs = commandCalls("metadata_search")[1]?.[1] as { requestId: string };
    unmount();
    expect(invoke).toHaveBeenCalledWith("metadata_cancel_request", {
      lane: "searchResults",
      requestId: secondArgs.requestId,
    });
  });
});
