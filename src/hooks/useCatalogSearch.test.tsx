// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogTrack } from "../types";
import { useCatalogSearch } from "./useCatalogSearch";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function track(title: string): CatalogTrack {
  return {
    providerId: "test",
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

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

describe("useCatalogSearch", () => {
  it("keeps a slow older suggestion response from replacing the latest query", async () => {
    vi.useFakeTimers();
    const older = deferred<CatalogTrack[]>();
    const latest = deferred<CatalogTrack[]>();
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const query = (args as { query: string }).query;
      return (query === "older" ? older.promise : latest.promise) as never;
    });

    const { result, rerender } = renderHook(
      ({ query }) => useCatalogSearch(query),
      { initialProps: { query: "older" } },
    );
    await act(async () => { vi.advanceTimersByTime(210); });

    rerender({ query: "latest" });
    await act(async () => { vi.advanceTimersByTime(210); });
    await act(async () => { latest.resolve([track("latest result")]); });
    expect(result.current.suggestions[0]?.title).toBe("latest result");

    await act(async () => { older.resolve([track("older result")]); });
    expect(result.current.suggestions[0]?.title).toBe("latest result");
    expect(result.current.suggestionState).toBe("ready");
  });
});
