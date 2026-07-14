// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NARROW_LAYOUT_QUERY, useNarrowLayout } from "./useNarrowLayout";

type ChangeListener = (event: MediaQueryListEvent) => void;

function installMatchMedia(initial: boolean) {
  let matches = initial;
  const listeners = new Set<ChangeListener>();
  const media = {
    get matches() {
      return matches;
    },
    media: NARROW_LAYOUT_QUERY,
    onchange: null,
    addEventListener: vi.fn((_type: string, listener: EventListenerOrEventListenerObject) => {
      if (typeof listener === "function") listeners.add(listener as ChangeListener);
    }),
    removeEventListener: vi.fn((_type: string, listener: EventListenerOrEventListenerObject) => {
      if (typeof listener === "function") listeners.delete(listener as ChangeListener);
    }),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(() => true),
  } as unknown as MediaQueryList;
  vi.stubGlobal("matchMedia", vi.fn(() => media));
  return {
    media,
    set(next: boolean) {
      matches = next;
      const event = { matches: next, media: NARROW_LAYOUT_QUERY } as MediaQueryListEvent;
      listeners.forEach((listener) => listener(event));
    },
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("useNarrowLayout", () => {
  it("tracks the 720px media query without changing desktop sidebar preference", () => {
    const match = installMatchMedia(false);
    const { result, unmount } = renderHook(() => useNarrowLayout());
    expect(result.current).toBe(false);
    expect(window.matchMedia).toHaveBeenCalledWith(NARROW_LAYOUT_QUERY);

    act(() => match.set(true));
    expect(result.current).toBe(true);

    act(() => match.set(false));
    expect(result.current).toBe(false);

    unmount();
    expect(match.media.removeEventListener).toHaveBeenCalledOnce();
  });
});
