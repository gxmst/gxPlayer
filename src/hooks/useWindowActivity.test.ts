// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { windowCanAnimate } from "./useWindowActivity";

const getCurrentWindow = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow }));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  getCurrentWindow.mockReset();
});

const activeWindow = {
  documentVisible: true,
  focused: true,
  minimized: false,
  visible: true,
};

describe("windowCanAnimate", () => {
  it("allows animation only for a visible focused window", () => {
    expect(windowCanAnimate(activeWindow)).toBe(true);
    expect(windowCanAnimate({ ...activeWindow, documentVisible: false })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, focused: false })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, minimized: true })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, visible: false })).toBe(false);
  });
});

describe("useWindowActivity", () => {
  it("tracks browser focus and visibility without reading Tauri window internals", async () => {
    const { useWindowActivity } = await import("./useWindowActivity");
    let hidden = false;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.stubGlobal("isTauri", false);
    vi.stubGlobal("__TAURI_INTERNALS__", undefined);

    const { result, unmount } = renderHook(() => useWindowActivity());
    expect(result.current).toBe(true);
    expect(getCurrentWindow).not.toHaveBeenCalled();

    act(() => window.dispatchEvent(new Event("blur")));
    expect(result.current).toBe(false);

    act(() => window.dispatchEvent(new Event("focus")));
    expect(result.current).toBe(true);

    hidden = true;
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(result.current).toBe(false);

    unmount();
    expect(getCurrentWindow).not.toHaveBeenCalled();
  });
});
