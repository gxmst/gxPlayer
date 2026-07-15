// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EMPTY_ENGINE } from "./types";

const runtime = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(async () => () => undefined),
}));

vi.mock("./lib/tauriClient", () => ({
  invoke: runtime.invoke,
  listen: runtime.listen,
  open: vi.fn(async () => null),
  save: vi.fn(async () => null),
  isBrowserMockRuntime: () => true,
  getCurrentWindow: () => ({
    minimize: async () => undefined,
    toggleMaximize: async () => undefined,
    close: async () => undefined,
    outerPosition: async () => ({ x: 0, y: 0 }),
    isMaximized: async () => false,
    isFocused: async () => true,
    isMinimized: async () => false,
    isVisible: async () => true,
    onResized: async () => () => undefined,
    onMoved: async () => () => undefined,
    onFocusChanged: async () => () => undefined,
  }),
}));

import App from "./App";

const localTrack = {
  id: 7,
  path: "C:/Music/City Lights.flac",
  title: "City Lights",
  artist: "GX Ensemble",
  album: "Night Drive",
  durationSeconds: 248,
  favorite: false,
  addedAtMs: 1_700_000_000_000,
  missing: false,
};

beforeEach(() => {
  const storage = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, String(value)),
      removeItem: (key: string) => storage.delete(key),
      clear: () => storage.clear(),
      key: (index: number) => [...storage.keys()][index] ?? null,
      get length() { return storage.size; },
    },
  });
  runtime.invoke.mockReset();
  runtime.listen.mockClear();
  runtime.invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "player_snapshot": return EMPTY_ENGINE;
      case "library_tracks":
      case "library_scan_missing": return [localTrack];
      case "library_favorites":
      case "library_playlists":
      case "library_history":
      case "cache_online_favorites":
      case "cache_list_entries":
      case "source_list":
      case "diagnostic_log_recent":
      case "metadata_search": return [];
      case "source_runtime_status": return { state: "ready", generation: 1, detail: null };
      case "cache_status": return { directory: "mock", totalBytes: 0, entryCount: 0, pinnedCount: 0, limitBytes: 5 * 1024 ** 3 };
      case "app_preferences_get": return { version: 1, closeBehavior: "hide_to_tray", closeToTrayNoticeShown: true, volume: 0.7, outputDevice: null };
      case "player_refresh_output_devices": return { devices: [], defaultDevice: null, selectedDevice: null };
      case "network_proxy_status": return { mode: "auto", detected: false };
      case "diagnostic_log_status": return { enabled: true };
      default: return undefined;
    }
  });
});

afterEach(() => cleanup());

describe("App shell", () => {
  it("navigates to the daily-use library controls", async () => {
    render(<App />);
    fireEvent.click(await screen.findByTitle("曲库"));
    expect(await screen.findByRole("heading", { name: "曲库" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("搜索本地歌曲、歌手、专辑或路径")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "导入文件夹" })).toBeInTheDocument();
  });

  it("shows local matches before online suggestions", async () => {
    render(<App />);
    const input = await screen.findByRole("combobox", { name: "搜索歌曲、歌手、专辑" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "City" } });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    await waitFor(() => expect(screen.getByText("本地曲库")).toBeInTheDocument());
    expect(screen.getByRole("option", { name: /City Lights.*本地/ })).toBeInTheDocument();
  });
});
