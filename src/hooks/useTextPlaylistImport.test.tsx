// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogTrack } from "../types";
import {
  createTextPlaylistSearch,
  useTextPlaylistImport,
  type TextPlaylistSearch,
} from "./useTextPlaylistImport";

function track(title: string, artist = "歌手"): CatalogTrack {
  return {
    providerId: "provider",
    providerTrackId: title,
    title,
    artist,
    album: "专辑",
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("useTextPlaylistImport", () => {
  it("runs one logical search at a time, reuses duplicate queries, and reports misses", async () => {
    let active = 0;
    let peak = 0;
    const calls: string[] = [];
    const search: TextPlaylistSearch = vi.fn(async (query) => {
      calls.push(query);
      active += 1;
      peak = Math.max(peak, active);
      await Promise.resolve();
      active -= 1;
      return query === "找不到" ? [] : [track(query)];
    });
    const { result } = renderHook(() => useTextPlaylistImport(search, { delayMs: 0 }));

    await act(async () => {
      await result.current.start("第一首\n找不到\n第一首");
    });

    expect(calls).toEqual(["第一首", "找不到"]);
    expect(peak).toBe(1);
    expect(result.current.state.phase).toBe("complete");
    expect(result.current.state.matched).toBe(2);
    expect(result.current.state.rows.map((row) => row.status)).toEqual(["matched", "not_found", "matched"]);
  });

  it("keeps search failures visible and continues with later lines", async () => {
    const search: TextPlaylistSearch = vi.fn(async (query) => {
      if (query === "失败") throw new Error("服务暂不可用");
      return [track(query)];
    });
    const { result } = renderHook(() => useTextPlaylistImport(search, { delayMs: 0 }));

    await act(async () => {
      await result.current.start("失败\n正常");
    });

    expect(result.current.state.rows[0]).toMatchObject({ status: "error", error: "服务暂不可用" });
    expect(result.current.state.rows[1]).toMatchObject({ status: "matched", track: track("正常") });
    expect(result.current.state.unresolved).toBe(1);
  });

  it("spaces distinct searches while letting cached duplicates proceed immediately", async () => {
    vi.useFakeTimers();
    const first = deferred<readonly CatalogTrack[]>();
    const search: TextPlaylistSearch = vi.fn((query) => query === "第一首"
      ? first.promise
      : Promise.resolve([track(query)]));
    const { result } = renderHook(() => useTextPlaylistImport(search, { delayMs: 100 }));

    let run!: Promise<unknown>;
    act(() => { run = result.current.start("第一首\n第二首\n第二首"); });
    expect(search).toHaveBeenCalledTimes(1);
    first.resolve([track("第一首")]);
    await act(async () => { await Promise.resolve(); });
    expect(search).toHaveBeenCalledTimes(1);

    await act(async () => { vi.advanceTimersByTime(99); await Promise.resolve(); });
    expect(search).toHaveBeenCalledTimes(1);
    await act(async () => { vi.advanceTimersByTime(1); await Promise.resolve(); await Promise.resolve(); });
    expect(search).toHaveBeenCalledTimes(2);
    await act(async () => { await run; });
    expect(search).toHaveBeenCalledTimes(2);
  });

  it("cancels the active line and never starts the next line", async () => {
    const pending = deferred<readonly CatalogTrack[]>();
    const search: TextPlaylistSearch = vi.fn(() => pending.promise);
    const { result } = renderHook(() => useTextPlaylistImport(search, { delayMs: 0 }));
    let run: Promise<unknown> | undefined;

    await act(async () => {
      run = result.current.start("当前\n下一首");
      await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
    });
    act(() => result.current.cancel());
    pending.resolve([track("当前")]);
    await act(async () => { await run; });

    expect(search).toHaveBeenCalledTimes(1);
    expect(result.current.state.phase).toBe("cancelled");
    expect(result.current.state.rows.map((row) => row.status)).toEqual(["cancelled", "cancelled"]);
  });

  it("does not search rejected URL lines", async () => {
    const search: TextPlaylistSearch = vi.fn(async (query) => [track(query)]);
    const { result } = renderHook(() => useTextPlaylistImport(search, { delayMs: 0 }));

    await act(async () => { await result.current.start("https://example.invalid/list\n歌曲"); });

    expect(search).toHaveBeenCalledTimes(1);
    expect(result.current.state.rows[0]).toMatchObject({ status: "invalid", error: "不支持链接格式，请输入歌曲文本" });
    expect(result.current.state.rows[1]?.status).toBe("matched");
  });
});

describe("createTextPlaylistSearch", () => {
  it("injects the existing metadata command and bounds the candidate count", async () => {
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      expect(command).toBe("metadata_search");
      expect(args).toEqual({ query: "歌曲", limit: 7 });
      return [track("歌曲")] as T;
    });
    const search = createTextPlaylistSearch(invoke, 7);

    await expect(search("歌曲", new AbortController().signal)).resolves.toEqual([track("歌曲")]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("does not invoke after cancellation is already requested", async () => {
    const invoke = vi.fn(async () => []);
    const search = createTextPlaylistSearch(invoke);
    const controller = new AbortController();
    controller.abort();

    await expect(search("歌曲", controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    expect(invoke).not.toHaveBeenCalled();
  });
});
