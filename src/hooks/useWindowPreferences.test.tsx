// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useWindowPreferences } from "./useWindowPreferences";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  getCurrentWindow: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: tauri.getCurrentWindow }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("useWindowPreferences", () => {
  it("keeps browser development usable when Tauri internals are absent", async () => {
    vi.stubGlobal("isTauri", false);
    vi.stubGlobal("__TAURI_INTERNALS__", undefined);
    const onError = vi.fn();
    const { result } = renderHook(() => useWindowPreferences(onError));

    expect(result.current.alwaysOnTop).toBe(false);
    expect(result.current.miniMode).toBe(false);
    expect(result.current.isMaximized).toBe(false);
    expect(tauri.getCurrentWindow).not.toHaveBeenCalled();
    expect(tauri.invoke).not.toHaveBeenCalled();

    let alwaysOnTopChanged = true;
    let miniModeChanged = true;
    await act(async () => {
      alwaysOnTopChanged = await result.current.toggleAlwaysOnTop();
      miniModeChanged = await result.current.toggleMiniMode();
    });

    expect(alwaysOnTopChanged).toBe(false);
    expect(miniModeChanged).toBe(false);
    expect(onError).not.toHaveBeenCalled();
    expect(tauri.getCurrentWindow).not.toHaveBeenCalled();
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});
