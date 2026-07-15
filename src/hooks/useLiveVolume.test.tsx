// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useLiveVolume } from "./useLiveVolume";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

function deferredFailure() {
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((_resolve, fail) => { reject = fail; });
  return { promise, reject };
}

describe("useLiveVolume", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => (
      window.setTimeout(() => callback(performance.now()), 16)
    ));
    vi.stubGlobal("cancelAnimationFrame", (id: number) => window.clearTimeout(id));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("previews the latest slider value before pointer release", async () => {
    const applyVolume = vi.fn(async (_volume: number) => undefined);
    const commitVolume = vi.fn(async (_volume: number) => undefined);
    const { result } = renderHook(() => useLiveVolume(1, applyVolume, commitVolume, vi.fn()));

    act(() => {
      result.current.previewVolume(0.7);
      result.current.previewVolume(0.4);
      vi.advanceTimersByTime(16);
    });
    await act(async () => Promise.resolve());

    expect(applyVolume).toHaveBeenCalledTimes(1);
    expect(applyVolume).toHaveBeenCalledWith(0.4);
    expect(result.current.shownVolume).toBe(0.4);
  });

  it("keeps only the latest value while one update is in flight", async () => {
    const first = deferred();
    const applyVolume = vi.fn<(volume: number) => Promise<void>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(undefined);
    const commitVolume = vi.fn(async (_volume: number) => undefined);
    const { result } = renderHook(() => useLiveVolume(1, applyVolume, commitVolume, vi.fn()));

    act(() => {
      result.current.previewVolume(0.8);
      vi.advanceTimersByTime(16);
    });
    expect(applyVolume).toHaveBeenCalledWith(0.8);

    act(() => {
      result.current.previewVolume(0.5);
      result.current.previewVolume(0.2);
      vi.advanceTimersByTime(16);
    });
    expect(applyVolume).toHaveBeenCalledTimes(1);

    await act(async () => {
      first.resolve();
      await first.promise;
    });
    expect(applyVolume.mock.calls.map(([volume]) => volume)).toEqual([0.8, 0.2]);
  });

  it("flushes the exact final value without waiting for the next frame", async () => {
    const previewVolume = vi.fn(async (_volume: number) => undefined);
    const commitActualVolume = vi.fn(async (_volume: number) => undefined);
    const { result } = renderHook(() => useLiveVolume(1, previewVolume, commitActualVolume, vi.fn()));

    act(() => {
      result.current.previewVolume(0.6);
      result.current.commitVolume(0.35);
    });
    await act(async () => Promise.resolve());

    expect(previewVolume).not.toHaveBeenCalled();
    expect(commitActualVolume.mock.calls.map(([volume]) => volume)).toEqual([0.35]);
    expect(result.current.isAdjustingVolume).toBe(false);
  });

  it("does not mistake an old snapshot for acknowledgement after returning to the initial value", () => {
    const applyVolume = vi.fn(async (_volume: number) => undefined);
    const { result, rerender } = renderHook(
      ({ actual }) => useLiveVolume(actual, applyVolume, vi.fn(), vi.fn()),
      { initialProps: { actual: 1 } },
    );

    act(() => result.current.previewVolume(0.75));
    act(() => result.current.previewVolume(1));
    expect(result.current.isAdjustingVolume).toBe(true);
    expect(result.current.shownVolume).toBe(1);
    act(() => result.current.commitVolume(1));
    expect(result.current.isAdjustingVolume).toBe(false);

    rerender({ actual: 0.75 });
    expect(result.current.shownVolume).toBe(1);
    rerender({ actual: 1 });

    expect(result.current.isAdjustingVolume).toBe(false);
    expect(result.current.shownVolume).toBe(1);
  });

  it("does not report a failed update after unmount", async () => {
    const update = deferredFailure();
    const onError = vi.fn();
    const applyVolume = vi.fn((_volume: number) => update.promise);
    const { result, unmount } = renderHook(() => useLiveVolume(1, applyVolume, vi.fn(), onError));

    act(() => {
      result.current.previewVolume(0.5);
      vi.advanceTimersByTime(16);
    });
    unmount();
    await act(async () => {
      update.reject(new Error("late failure"));
      await update.promise.catch(() => undefined);
    });

    expect(onError).not.toHaveBeenCalled();
  });

  it("persists only the final trailing value after live previews", async () => {
    const previewVolume = vi.fn(async (_volume: number) => undefined);
    const commitVolume = vi.fn(async (_volume: number) => undefined);
    const { result } = renderHook(() => useLiveVolume(1, previewVolume, commitVolume, vi.fn()));

    act(() => {
      result.current.previewVolume(0.8);
      result.current.previewVolume(0.6);
      vi.advanceTimersByTime(16);
    });
    await act(async () => Promise.resolve());
    act(() => {
      result.current.previewVolume(0.4);
      vi.advanceTimersByTime(180);
    });
    await act(async () => Promise.resolve());

    expect(previewVolume.mock.calls.map(([volume]) => volume)).toEqual([0.6, 0.4]);
    expect(commitVolume.mock.calls.map(([volume]) => volume)).toEqual([0.4]);
  });
});
