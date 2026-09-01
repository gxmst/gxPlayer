// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { buildDspControlState } from "./lib/dspPresets";
import { EMPTY_ENGINE } from "./types";

const runtime = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(async (_event: string, _handler: (event: { payload: any }) => void) => () => undefined),
  open: vi.fn(async () => null as string | null),
  save: vi.fn(async () => null as string | null),
}));

vi.mock("./lib/tauriClient", () => ({
  invoke: runtime.invoke,
  listen: runtime.listen,
  open: runtime.open,
  save: runtime.save,
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

const defaultDspControl = buildDspControlState("bypass");
let cacheEntries: Array<Record<string, unknown>> = [];
let cacheExportResult: Promise<Array<Record<string, unknown>>> = Promise.resolve([]);

function appPreferences(dspControl = defaultDspControl) {
  return {
    version: 2,
    closeBehavior: "hide_to_tray",
    closeToTrayNoticeShown: true,
    volume: 0.7,
    outputDevice: null,
    dspControl,
    customEqPresets: [],
    chartRegion: "cn",
    chartAutoLoad: false,
  };
}

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
  runtime.open.mockReset();
  runtime.open.mockResolvedValue(null);
  runtime.save.mockReset();
  runtime.save.mockResolvedValue(null);
  cacheEntries = [];
  cacheExportResult = Promise.resolve([]);
  runtime.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    switch (command) {
      case "player_snapshot": return EMPTY_ENGINE;
      case "library_tracks":
      case "library_scan_missing": return [localTrack];
      case "library_favorites":
      case "library_playlists":
      case "library_history":
      case "cache_online_favorites":
      case "source_list":
      case "diagnostic_log_recent":
      case "metadata_search": return [];
      case "cache_list_entries": return cacheEntries;
      case "source_status": return { state: "ready", generation: 1, activeSourceId: null, capabilities: null, error: null, updateAlert: null };
      case "cache_status": return { directory: "mock", totalBytes: 0, entryCount: 0, pinnedCount: 0, limitBytes: 5 * 1024 ** 3 };
      case "app_preferences_get": return appPreferences();
      case "metadata_chart_regions": return ["cn", "us", "jp"];
      case "metadata_chart": return [];
      case "player_set_dsp_settings": return appPreferences(args?.control as ReturnType<typeof buildDspControlState>);
      case "player_set_ab_dry": return undefined;
      case "player_refresh_output_devices": return { devices: [], defaultDevice: null, selectedDevice: null };
      case "network_proxy_status": return { mode: "auto", detected: false };
      case "diagnostic_log_status": return { enabled: true };
      case "library_export_backup": return { version: 2, tracks: [], playlists: [] };
      case "source_export_backup": return { version: 1, sources: [] };
      case "cache_export_entries": return cacheExportResult;
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

  it("does not present an audio-source read failure as an empty source collection", async () => {
    const defaultInvoke = runtime.invoke.getMockImplementation();
    runtime.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "source_list") throw new Error("desktop IPC unavailable");
      return defaultInvoke?.(command, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByTitle("音源管理"));

    expect(await screen.findByRole("heading", { name: "音源读取失败" })).toBeInTheDocument();
    expect(screen.queryByText("还没有导入音源。可从本地文件或你提供的 URL 导入脚本。")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新读取" })).toBeInTheDocument();
  });

  it("does not present an online-favorite read failure as no saved favorites", async () => {
    const defaultInvoke = runtime.invoke.getMockImplementation();
    runtime.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "cache_online_favorites") throw new Error("desktop IPC unavailable");
      return defaultInvoke?.(command, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByTitle("收藏"));

    expect(await screen.findByRole("heading", { name: "收藏读取失败" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "还没有收藏" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新读取" })).toBeInTheDocument();
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

  it("keeps preset persistence and momentary A/B on separate command paths", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByTitle("设置与备份"))[0]);
    expect(await screen.findByRole("heading", { name: "音效预设" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: "人声" }));
    const vocal = buildDspControlState("vocal");
    await waitFor(() => {
      expect(runtime.invoke).toHaveBeenCalledWith("player_set_dsp_settings", { control: vocal });
    });
    expect(runtime.invoke.mock.calls.some(([command]) => command === "player_set_audio_mode")).toBe(false);

    runtime.invoke.mockClear();
    const compare = screen.getByRole("button", { name: "按住听未处理" });
    fireEvent.pointerDown(compare, { button: 0, pointerId: 9 });
    fireEvent.pointerUp(compare, { pointerId: 9 });

    await waitFor(() => {
      const abCalls = runtime.invoke.mock.calls.filter(([command]) => command === "player_set_ab_dry");
      expect(abCalls).toEqual([
        ["player_set_ab_dry", { enabled: true }],
        ["player_set_ab_dry", { enabled: false }],
      ]);
    });
    expect(runtime.invoke.mock.calls.some(([command]) => command === "player_set_dsp_settings")).toBe(false);
  });

  it("persists a chart region change and refetches only that region", async () => {
    render(<App />);
    // Auto-load is off in the mock preferences, so nothing has hit the network yet.
    await waitFor(() => expect(screen.getByRole("heading", { name: "正在流行" })).toBeInTheDocument());
    expect(runtime.invoke.mock.calls.some(([command]) => command === "metadata_chart")).toBe(false);

    runtime.invoke.mockClear();
    const picker = await screen.findByRole("combobox", { name: "榜单地区" });
    fireEvent.change(picker, { target: { value: "jp" } });

    await waitFor(() => {
      expect(runtime.invoke).toHaveBeenCalledWith("app_preferences_set_chart_region", { region: "jp" });
    });
    const chartCalls = runtime.invoke.mock.calls.filter(([command]) => command === "metadata_chart");
    expect(chartCalls).toEqual([["metadata_chart", { limit: 12, region: "jp" }]]);
  });

  it("exports a restorable v2 backup envelope", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByTitle("设置与备份"))[0]);
    fireEvent.click(await screen.findByRole("tab", { name: "高级" }));
    fireEvent.click(screen.getByRole("button", { name: "生成到文本框" }));

    const textarea = await screen.findByRole("textbox", { name: "GXPlayer 备份 JSON" });
    await waitFor(() => {
      const backup = JSON.parse((textarea as HTMLTextAreaElement).value) as {
        version: number;
        library: { version: number };
      };
      expect(backup.version).toBe(2);
      expect(backup.library.version).toBe(2);
    });
  });

  it("subscribes before cache export and reports progress until the copy finishes", async () => {
    cacheEntries = [
      {
        providerId: "demo",
        providerTrackId: "one",
        quality: "flac",
        title: "First",
        artist: "Artist",
        album: "Album",
        byteLen: 1024,
        sourceSampleRate: 48_000,
        sourceBitDepth: 24,
        sourceChannels: 2,
        mediaType: "flac",
        pinned: false,
        lastAccessedAtMs: 1_700_000_000_000,
        completedAtMs: 1_700_000_000_000,
        fileName: "cache-one",
      },
      {
        providerId: "demo",
        providerTrackId: "two",
        quality: "flac",
        title: "Second",
        artist: "Artist",
        album: "Album",
        byteLen: 2048,
        sourceSampleRate: 48_000,
        sourceBitDepth: 24,
        sourceChannels: 2,
        mediaType: "flac",
        pinned: false,
        lastAccessedAtMs: 1_700_000_000_000,
        completedAtMs: 1_700_000_000_000,
        fileName: "cache-two",
      },
    ];
    runtime.open.mockResolvedValue("C:/Exports");

    let reportProgress: ((event: { payload: { completed: number; total: number; current: string } }) => void) | null = null;
    const stopProgress = vi.fn();
    runtime.listen.mockImplementation(async (event: string, handler: (event: { payload: any }) => void) => {
      if (event === "gx-cache-export-progress") {
        reportProgress = handler;
        return stopProgress;
      }
      return () => undefined;
    });

    let finishExport: (outcomes: Array<Record<string, unknown>>) => void = () => undefined;
    cacheExportResult = new Promise((resolve) => { finishExport = resolve; });

    render(<App />);
    fireEvent.click(await screen.findByTitle("曲库"));
    fireEvent.click(await screen.findByRole("button", { name: "导出全部" }));

    await waitFor(() => expect(reportProgress).not.toBeNull());
    expect(runtime.invoke.mock.calls.some(([command]) => command === "cache_export_entries")).toBe(true);
    expect(screen.getByRole("progressbar", { name: "缓存导出进度" })).toHaveValue(0);

    act(() => {
      reportProgress?.({ payload: { completed: 1, total: 2, current: "Artist - Second" } });
    });
    expect(screen.getByText("正在导出 1 / 2")).toBeInTheDocument();
    expect(screen.getByText("正在写入 Artist - Second")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "缓存导出进度" })).toHaveValue(1);

    await act(async () => {
      finishExport([
        { providerId: "demo", providerTrackId: "one", quality: "flac", fileName: "Artist - First.flac", error: null },
        { providerId: "demo", providerTrackId: "two", quality: "flac", fileName: "Artist - Second.flac", error: null },
      ]);
      await cacheExportResult;
    });

    await waitFor(() => expect(screen.queryByRole("progressbar", { name: "缓存导出进度" })).not.toBeInTheDocument());
    expect(stopProgress).toHaveBeenCalledOnce();
  });
});
