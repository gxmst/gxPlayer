import { afterEach, describe, expect, it, vi } from "vitest";
import { hasTauriWindowRuntime } from "./tauriRuntime";

afterEach(() => vi.unstubAllGlobals());

describe("hasTauriWindowRuntime", () => {
  it("requires both the Tauri marker and usable window internals", () => {
    vi.stubGlobal("isTauri", false);
    vi.stubGlobal("__TAURI_INTERNALS__", undefined);
    expect(hasTauriWindowRuntime()).toBe(false);

    vi.stubGlobal("isTauri", true);
    vi.stubGlobal("__TAURI_INTERNALS__", { invoke: vi.fn() });
    expect(hasTauriWindowRuntime()).toBe(false);

    vi.stubGlobal("__TAURI_INTERNALS__", {
      invoke: vi.fn(),
      metadata: { currentWindow: { label: "main" } },
    });
    expect(hasTauriWindowRuntime()).toBe(true);
  });
});
