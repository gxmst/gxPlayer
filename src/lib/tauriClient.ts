import { invoke as nativeInvoke } from "@tauri-apps/api/core";
import { listen as nativeListen, type Event, type EventCallback, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow as nativeGetCurrentWindow } from "@tauri-apps/api/window";
import { open as nativeOpen, save as nativeSave } from "@tauri-apps/plugin-dialog";
import { EMPTY_ENGINE } from "../types";
import { hasTauriWindowRuntime } from "./tauriRuntime";

const TEST_MODE = import.meta.env.MODE === "test";

const demoLibrary = [
  {
    id: 1,
    path: "C:/Music/City Lights.flac",
    title: "City Lights",
    artist: "GX Ensemble",
    album: "Night Drive",
    durationSeconds: 248,
    favorite: true,
    addedAtMs: Date.now() - 86_400_000,
    missing: false,
  },
  {
    id: 2,
    path: "C:/Music/Quiet Room.wav",
    title: "Quiet Room",
    artist: "Aster",
    album: "Home Sessions",
    durationSeconds: 193,
    favorite: false,
    addedAtMs: Date.now() - 172_800_000,
    missing: false,
  },
];

const demoCatalog = [
  {
    providerId: "browser-mock",
    providerTrackId: "demo-1",
    title: "Afterglow",
    artist: "Luna North",
    album: "Signals",
    durationMs: 221_000,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  },
];

function mockResult(command: string, args?: Record<string, unknown>): unknown {
  switch (command) {
    case "player_snapshot":
      return EMPTY_ENGINE;
    case "library_tracks":
    case "library_scan_missing":
      return demoLibrary;
    case "library_favorites":
      return demoLibrary.filter((track) => track.favorite);
    case "library_playlists":
    case "library_history":
    case "library_playlist_items":
    case "cache_online_favorites":
    case "cache_list_entries":
    case "source_list":
    case "diagnostic_log_recent":
      return [];
    case "metadata_search":
    case "metadata_chart":
      return demoCatalog;
    case "metadata_lyrics":
      return {
        instrumental: false,
        lines: [
          { timestampMs: 0, text: "浏览器模式使用演示数据" },
          { timestampMs: 8_000, text: "桌面端会连接真实曲库与播放引擎" },
        ],
      };
    case "source_runtime_status":
      return { state: "ready", generation: 1, detail: "浏览器演示模式" };
    case "cache_status":
      return { directory: "浏览器演示", totalBytes: 0, entryCount: 0, pinnedCount: 0, limitBytes: 5 * 1024 ** 3 };
    case "preview_cache_status":
      return { totalBytes: 0, entryCount: 0, limitBytes: 256 * 1024 ** 2 };
    case "app_preferences_get":
      return { version: 1, closeBehavior: "hide_to_tray", closeToTrayNoticeShown: true, volume: 0.72, outputDevice: null };
    case "player_refresh_output_devices":
      return { devices: ["浏览器演示设备"], defaultDevice: "浏览器演示设备", selectedDevice: null };
    case "network_proxy_status":
    case "network_set_proxy_mode":
      return { mode: args?.mode ?? "auto", detected: false };
    case "diagnostic_log_status":
      return { enabled: true };
    case "library_embedded_cover":
      return null;
    case "library_import_files":
      return { imported: [], failures: [] };
    case "library_import_folders":
      return { imported: [], failures: [], scannedFileCount: 0, skippedFileCount: 0 };
    case "library_remove_tracks":
      return { removedTrackIds: args?.trackIds ?? [] };
    case "library_relink_tracks":
      return { relinked: [], failures: [] };
    case "backup_export":
      return JSON.stringify({ version: 1, browserMock: true }, null, 2);
    case "backup_preview_restore":
    case "backup_restore_atomic":
      return { trackCount: 0, playlistCount: 0, sourceCount: 0 };
    default:
      return undefined;
  }
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (TEST_MODE || hasTauriWindowRuntime()) return nativeInvoke<T>(command, args);
  return mockResult(command, args) as T;
}

export function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  if (TEST_MODE || hasTauriWindowRuntime()) return nativeListen<T>(event, handler);
  void event;
  void handler;
  return Promise.resolve(() => undefined);
}

const browserWindow = {
  minimize: async () => undefined,
  toggleMaximize: async () => undefined,
  close: async () => undefined,
  isMaximized: async () => false,
  isFocused: async () => document.hasFocus(),
  isMinimized: async () => false,
  isVisible: async () => true,
  outerPosition: async () => ({ x: 0, y: 0 }),
  onResized: async () => () => undefined,
  onMoved: async () => () => undefined,
  onFocusChanged: async (handler: EventCallback<boolean>) => {
    const onFocus = () => handler({ event: "focus", id: 0, payload: true } as Event<boolean>);
    const onBlur = () => handler({ event: "focus", id: 0, payload: false } as Event<boolean>);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
    };
  },
};

export function getCurrentWindow() {
  return TEST_MODE || hasTauriWindowRuntime() ? nativeGetCurrentWindow() : browserWindow;
}

export async function open(options?: Parameters<typeof nativeOpen>[0]) {
  if (TEST_MODE || hasTauriWindowRuntime()) return nativeOpen(options);
  return null;
}

export async function save(options?: Parameters<typeof nativeSave>[0]) {
  if (TEST_MODE || hasTauriWindowRuntime()) return nativeSave(options);
  return null;
}

export function isBrowserMockRuntime(): boolean {
  return !TEST_MODE && !hasTauriWindowRuntime();
}
