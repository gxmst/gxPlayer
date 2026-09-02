import { afterEach, describe, expect, it, vi } from "vitest";
import { hasTauriWindowRuntime } from "./tauriRuntime";

afterEach(() => vi.unstubAllGlobals());

describe("hasTauriWindowRuntime", () => {
  it("requires usable IPC internals without trusting the legacy marker", () => {
    vi.stubGlobal("isTauri", false);
    vi.stubGlobal("__TAURI_INTERNALS__", undefined);
    expect(hasTauriWindowRuntime()).toBe(false);

    vi.stubGlobal("__TAURI_INTERNALS__", { invoke: vi.fn() });
    expect(hasTauriWindowRuntime()).toBe(true);

    vi.stubGlobal("__TAURI_INTERNALS__", {
      invoke: vi.fn(),
      metadata: { currentWindow: { label: "main" } },
    });
    expect(hasTauriWindowRuntime()).toBe(true);
  });

  it("recognizes the packaged Tauri host before IPC globals finish initializing", () => {
    vi.stubGlobal("__TAURI_INTERNALS__", undefined);
    vi.stubGlobal("location", { hostname: "tauri.localhost", protocol: "http:" });
    expect(hasTauriWindowRuntime()).toBe(true);
  });
});
