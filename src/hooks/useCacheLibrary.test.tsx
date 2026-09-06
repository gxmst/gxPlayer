// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useCacheLibrary } from "./useCacheLibrary";
import type { CacheEntryPage, LibraryPlaylistItem, ViewId } from "../types";

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("../lib/tauriClient", () => ({ invoke, listen }));
const page = (offset: number): CacheEntryPage => ({ entries: [], offset, totalCount: 201, qualities: ["studio"] });
const items: LibraryPlaylistItem[] = [{ kind: "cached", providerId: "custom", providerTrackId: "1", quality: "studio", title: "Song", artist: "Artist", album: "" }];

describe("cache library loading", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
    listen.mockResolvedValue(() => undefined);
    invoke.mockImplementation(async (command, args) => {
      if (command === "cache_status") return { entryCount: 201, limitBytes: 1024 };
      if (command === "cache_online_favorites") return [];
      if (command === "cache_list_page") return page(args.offset);
      if (command === "cache_available_keys") return args.keys;
      throw new Error(`unexpected command: ${command}`);
    });
  });
  afterEach(cleanup);

  it("reads favorites without fetching cache rows and checks playlist keys separately", async () => {
    const { result, rerender } = renderHook(({ view }: { view: ViewId }) => useCacheLibrary(view, items), { initialProps: { view: "favorites" as ViewId } });
    await waitFor(() => expect(result.current.cacheLoadState).toBe("ready"));
    expect(invoke.mock.calls.some(([command]) => command === "cache_list_page")).toBe(false);
    rerender({ view: "playlist" });
    await waitFor(() => expect(result.current.cacheAvailabilityState).toBe("ready"));
    expect(result.current.availableCacheKeys.has("custom\u00001\u0000studio")).toBe(true);
    expect(invoke.mock.calls.filter(([command]) => command === "cache_available_keys")).toHaveLength(1);
  });

  it("does not let an older page request replace a newer page", async () => {
    let finishOld!: (value: CacheEntryPage) => void;
    const base = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === "cache_list_page" && args.offset === 100
      ? new Promise((resolve) => { finishOld = resolve; }) : base?.(command, args));
    const { result } = renderHook(() => useCacheLibrary("library", items));
    await waitFor(() => expect(result.current.cachePageLoadState).toBe("ready"));
    let old!: Promise<void>;
    await act(async () => { old = result.current.loadCachePage(100); });
    await act(async () => { await result.current.loadCachePage(200); });
    await act(async () => { finishOld(page(100)); await old; });
    expect(result.current.cachePage.offset).toBe(200);
  });

  it("does not let a page response after leaving the library change its state", async () => {
    let finish!: (value: CacheEntryPage) => void;
    const base = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === "cache_list_page"
      ? new Promise((resolve) => { finish = resolve; }) : base?.(command, args));
    const { result, rerender } = renderHook(({ view }: { view: ViewId }) => useCacheLibrary(view, items), { initialProps: { view: "library" as ViewId } });
    rerender({ view: "discovery" });
    await act(async () => { finish(page(100)); });
    expect(result.current.cachePage.offset).toBe(0);
  });

  it("keeps the requested page when a refresh arrives during pagination", async () => {
    const pending: Array<(page: CacheEntryPage) => void> = [];
    const base = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === "cache_list_page" && args.offset === 100
      ? new Promise((resolve) => { pending.push(resolve); }) : base?.(command, args));
    const { result } = renderHook(() => useCacheLibrary("library", items));
    await waitFor(() => expect(result.current.cachePageLoadState).toBe("ready"));
    let turnPage!: Promise<void>;
    let refresh!: Promise<void>;
    await act(async () => { turnPage = result.current.loadCachePage(100); });
    await act(async () => { refresh = result.current.refreshCache(); });
    expect(pending).toHaveLength(2);
    await act(async () => { pending[1](page(100)); await refresh; pending[0](page(100)); await turnPage; });
    expect(result.current.cachePage.offset).toBe(100);
  });
});
