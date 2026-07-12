import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "@fontsource-variable/noto-sans-sc";
import gxplayerIcon from "./assets/gxplayer-icon.png";
import "./App.css";
import { QueuePanel } from "./components/QueuePanel";
import { ResolveBanner } from "./components/ResolveBanner";
import { SourceGuide } from "./components/SourceGuide";
import { useCatalogSearch } from "./hooks/useCatalogSearch";
import { useEngineSnapshot } from "./hooks/useEngineSnapshot";
import { useWindowPreferences } from "./hooks/useWindowPreferences";
import {
  frontendNextIndex,
  moveIndex,
  pickFailureSkipIndex,
} from "./lib/playlistLogic";
import { formatFailureMessage } from "./lib/resolveErrors";
import {
  STARTED,
  nextOptionIndex,
  putLruValue,
  shouldSkipAfterStart,
  type PlaybackStartResult,
} from "./lib/uiState";
import {
  deriveTransportCapabilities,
  isTransportAction,
  type TransportAction,
  type TransportCapabilities,
} from "./lib/transport";
import {
  type CacheEntryView,
  type CacheStatus,
  type CatalogTrack,
  type EngineSnapshot,
  type HistoryEntry,
  type LibraryImportResult,
  type LibraryTrack,
  type ListedSource,
  type LyricDocument,
  type OnlinePlaybackResult,
  type PlayMode,
  type PlaylistSummary,
  type ResolveAttemptDiagnostic,
  type RuntimeStatus,
  type SourceFallbackConfig,
  type ViewId,
} from "./types";

type AudioMode = EngineSnapshot["audioMode"];
type QualityPreference = "auto" | "128k" | "320k" | "flac" | "flac24bit";
type SourceConfigDraft = {
  lsConfig: Record<string, unknown>;
  constName: string;
  keyValue: string;
  apiAddr: string;
  apiPass: string;
};
type SearchOption =
  | { id: string; kind: "track"; track: CatalogTrack }
  | { id: string; kind: "artist" | "album"; query: string }
  | { id: string; kind: "all" };

/** Frontend playlist entry. Online items store metadata only — never pre-resolved URLs. */
type PlaylistEntry =
  | {
      kind: "local";
      path: string;
      title: string;
      artist: string;
      durationSeconds: number | null;
    }
  | {
      kind: "online";
      track: CatalogTrack;
      quality: QualityPreference;
    }
  | {
      /** Completed online cache — play via local file, no LX resolve. */
      kind: "cached";
      providerId: string;
      providerTrackId: string;
      quality: string;
      title: string;
      artist: string;
    };

const PLAY_MODE_ORDER: PlayMode[] = ["sequential", "repeat_all", "repeat_one", "shuffle"];
const PLAY_MODE_META: Record<PlayMode, { label: string; glyph: string }> = {
  sequential: { label: "顺序播放", glyph: "seq" },
  repeat_all: { label: "列表循环", glyph: "all" },
  repeat_one: { label: "单曲循环", glyph: "one" },
  shuffle: { label: "随机播放", glyph: "shuf" },
};
const RESOLVE_TIMEOUT_MS = 25_000;
const TOAST_OK_MS = 3_000;
const TOAST_ERROR_MS = 10_000;
const COVER_CACHE_LIMIT = 96;

function catalogKey(track: CatalogTrack): string {
  return `${track.providerId}:${track.providerTrackId}`;
}

function entryKey(entry: PlaylistEntry, index: number): string {
  if (entry.kind === "local") return `local:${entry.path}:${index}`;
  if (entry.kind === "cached") {
    return `cached:${entry.providerId}:${entry.providerTrackId}:${entry.quality}:${index}`;
  }
  return `online:${catalogKey(entry.track)}:${index}`;
}

function entryTitle(entry: PlaylistEntry): string {
  if (entry.kind === "local" || entry.kind === "cached") return entry.title;
  return entry.track.title;
}

function entryArtist(entry: PlaylistEntry): string {
  if (entry.kind === "local") return entry.artist || "未知歌手";
  if (entry.kind === "cached") return entry.artist || "未知歌手";
  return entry.track.artist || "未知歌手";
}

function entrySourceLabel(entry: PlaylistEntry): string {
  if (entry.kind === "local") return "本地";
  if (entry.kind === "cached") return `缓存 · ${entry.quality}`;
  return "在线";
}

function cacheEntryToPlaylist(entry: CacheEntryView): PlaylistEntry {
  return {
    kind: "cached",
    providerId: entry.providerId,
    providerTrackId: entry.providerTrackId,
    quality: entry.quality,
    title: entry.title,
    artist: entry.artist,
  };
}

function cacheEntryToCatalog(entry: CacheEntryView): CatalogTrack {
  return {
    providerId: entry.providerId,
    providerTrackId: entry.providerTrackId,
    title: entry.title,
    artist: entry.artist,
    album: entry.album,
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}

function localEntryFromLibrary(track: LibraryTrack): PlaylistEntry {
  return {
    kind: "local",
    path: track.path,
    title: track.title,
    artist: track.artist,
    durationSeconds: track.durationSeconds,
  };
}

function onlineEntryFromCatalog(track: CatalogTrack, quality: QualityPreference): PlaylistEntry {
  return { kind: "online", track, quality };
}

function playlistIsLocalOnly(entries: PlaylistEntry[]): boolean {
  return entries.length > 0 && entries.every((entry) => entry.kind === "local");
}

const QUALITY_OPTIONS: Array<{ value: QualityPreference; label: string }> = [
  { value: "auto", label: "自动" },
  { value: "128k", label: "128k" },
  { value: "320k", label: "320k" },
  { value: "flac", label: "无损 FLAC" },
  { value: "flac24bit", label: "24-bit FLAC" },
];

/** Premium rose — clean on dark glass; used when there is no artwork. */
const FALLBACK_ACCENT = "#e85a71";
/** Curated accents only — never raw hash hues that land on muddy yellow-green. */
const PREMIUM_PALETTE = [
  "#e85a71",
  "#7b8cff",
  "#5ec8c8",
  "#c77dff",
  "#ff8e6e",
  "#6ec8ff",
  "#f0a0c0",
  "#8ad4a0",
  "#d4a06a",
  "#a78bfa",
] as const;
const NAV_ITEMS: Array<{ id: ViewId; icon: string; label: string }> = [
  { id: "discovery", icon: "⌂", label: "探索" },
  { id: "library", icon: "♫", label: "曲库" },
  { id: "history", icon: "◷", label: "播放历史" },
  { id: "favorites", icon: "♥", label: "收藏" },
  { id: "sources", icon: "◈", label: "音源管理" },
  { id: "settings", icon: "⚙", label: "设置与备份" },
];

function initialView(): ViewId {
  const requested = new URLSearchParams(window.location.search).get("view");
  // offline merged into library; search is top-bar only (results page still reachable).
  if (requested === "offline") return "library";
  return requested && ["discovery", "search", "library", "history", "favorites", "playlist", "sources", "settings", "now-playing"].includes(requested)
    ? (requested as ViewId)
    : "discovery";
}

function formatTime(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "--:--";
  const value = Math.max(0, Math.floor(seconds));
  return `${Math.floor(value / 60)}:${(value % 60).toString().padStart(2, "0")}`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

function formatSourceSpec(snapshot: EngineSnapshot): string | null {
  if (!snapshot.sourceSampleRate && !snapshot.sourceBitDepth && !snapshot.sourceChannels) return null;
  const parts: string[] = [];
  if (snapshot.sourceSampleRate) {
    const khz = snapshot.sourceSampleRate / 1000;
    parts.push(`${Number.isInteger(khz) ? khz.toFixed(0) : khz.toFixed(1)}kHz`);
  }
  if (snapshot.sourceBitDepth) parts.push(`${snapshot.sourceBitDepth}bit`);
  if (snapshot.sourceChannels) parts.push(`${snapshot.sourceChannels}ch`);
  return parts.join("/");
}

function formatResolveAttempts(attempts: ResolveAttemptDiagnostic[]): string {
  if (!attempts.length) return "";
  const failed = attempts.filter((attempt) => !attempt.success);
  const last = failed[failed.length - 1] ?? attempts[attempts.length - 1];
  const location = [last?.sourceName || last?.sourceId, last?.quality].filter(Boolean).join(" · ");
  return `（共 ${attempts.length} 次尝试${location ? `，最后：${location}` : ""}）`;
}

function isSuspiciousQuality(quality: string | null, snapshot: EngineSnapshot): boolean {
  if (!quality) return false;
  const normalized = quality.toLowerCase();
  if (normalized === "flac24bit" || normalized.includes("24bit")) {
    return snapshot.sourceBitDepth !== null
      && snapshot.sourceBitDepth !== undefined
      && snapshot.sourceBitDepth <= 16;
  }
  if (normalized.includes("hires") || normalized.includes("hi-res")) {
    const lowDepth = snapshot.sourceBitDepth !== null
      && snapshot.sourceBitDepth !== undefined
      && snapshot.sourceBitDepth <= 16;
    const lowRate = snapshot.sourceSampleRate !== null
      && snapshot.sourceSampleRate !== undefined
      && snapshot.sourceSampleRate <= 48_000;
    return lowDepth && lowRate;
  }
  return false;
}

function initials(title: string): string {
  return [...(title.trim() || "GX")].slice(0, 2).join("").toUpperCase();
}

function hashString(key: string): number {
  let hash = 0;
  for (const character of key) hash = (hash * 31 + character.charCodeAt(0)) | 0;
  return Math.abs(hash);
}

function fallbackAccent(key: string): string {
  if (!key.trim()) return FALLBACK_ACCENT;
  return PREMIUM_PALETTE[hashString(key) % PREMIUM_PALETTE.length];
}

type Rgb = { r: number; g: number; b: number };
type Hsl = { h: number; s: number; l: number };

function rgbToHsl(r: number, g: number, b: number): Hsl {
  const red = r / 255;
  const green = g / 255;
  const blue = b / 255;
  const max = Math.max(red, green, blue);
  const min = Math.min(red, green, blue);
  const lightness = (max + min) / 2;
  if (max === min) return { h: 0, s: 0, l: lightness };
  const delta = max - min;
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min);
  let hue = 0;
  if (max === red) hue = ((green - blue) / delta + (green < blue ? 6 : 0)) / 6;
  else if (max === green) hue = ((blue - red) / delta + 2) / 6;
  else hue = ((red - green) / delta + 4) / 6;
  return { h: hue * 360, s: saturation, l: lightness };
}

function hslToRgb(h: number, s: number, l: number): Rgb {
  const hue = ((h % 360) + 360) % 360;
  const saturation = Math.min(1, Math.max(0, s));
  const lightness = Math.min(1, Math.max(0, l));
  if (saturation === 0) {
    const gray = Math.round(lightness * 255);
    return { r: gray, g: gray, b: gray };
  }
  const q = lightness < 0.5 ? lightness * (1 + saturation) : lightness + saturation - lightness * saturation;
  const p = 2 * lightness - q;
  const toChannel = (t: number) => {
    let value = t;
    if (value < 0) value += 1;
    if (value > 1) value -= 1;
    if (value < 1 / 6) return p + (q - p) * 6 * value;
    if (value < 1 / 2) return q;
    if (value < 2 / 3) return p + (q - p) * (2 / 3 - value) * 6;
    return p;
  };
  const hk = hue / 360;
  return {
    r: Math.round(toChannel(hk + 1 / 3) * 255),
    g: Math.round(toChannel(hk) * 255),
    b: Math.round(toChannel(hk - 1 / 3) * 255),
  };
}

/** Push any extracted color into a clean, luminous accent range for dark UI. */
function polishAccent(r: number, g: number, b: number): string {
  const { h, s, l } = rgbToHsl(r, g, b);
  // Floor saturation/lightness so accents stay vivid on glass; cap so they never scream.
  const nextS = Math.min(0.78, Math.max(0.52, s < 0.2 ? 0.58 : s * 1.12));
  const nextL = Math.min(0.66, Math.max(0.52, l < 0.35 ? 0.58 : l > 0.72 ? 0.6 : l));
  const polished = hslToRgb(h, nextS, nextL);
  return `rgb(${polished.r} ${polished.g} ${polished.b})`;
}

/**
 * Extract a vivid, clean dominant accent from cover art.
 * Prefers saturated mid-lightness pixels over full-image averages that go muddy/dark.
 */
async function accentFromArtwork(url: string | null, key: string): Promise<string> {
  if (!url) return FALLBACK_ACCENT;
  return new Promise((resolve) => {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onload = () => {
      try {
        const size = 64;
        const canvas = document.createElement("canvas");
        canvas.width = size;
        canvas.height = size;
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (!context) return resolve(fallbackAccent(key));
        context.drawImage(image, 0, 0, size, size);
        const { data } = context.getImageData(0, 0, size, size);

        // 36 hue bins × weighted RGB accumulation for dominant vibrant color.
        const bins = Array.from({ length: 36 }, () => ({
          weight: 0,
          r: 0,
          g: 0,
          b: 0,
        }));

        for (let index = 0; index < data.length; index += 4) {
          const alpha = data[index + 3];
          if (alpha < 200) continue;
          const red = data[index];
          const green = data[index + 1];
          const blue = data[index + 2];
          const { h, s, l } = rgbToHsl(red, green, blue);
          // Drop near-black, near-white, and low-chroma mud.
          if (l < 0.12 || l > 0.9 || s < 0.12) continue;
          // Prefer saturated mid-tones; lightly de-weight yellow-green mud bands.
          const chroma = s * (1 - Math.abs(l - 0.52) * 1.4);
          const mudPenalty = h >= 55 && h <= 100 && s < 0.45 ? 0.35 : 1;
          const score = chroma * chroma * mudPenalty;
          if (score < 0.01) continue;
          const bin = Math.min(35, Math.floor(h / 10));
          bins[bin].weight += score;
          bins[bin].r += red * score;
          bins[bin].g += green * score;
          bins[bin].b += blue * score;
        }

        let best = bins[0];
        for (const bin of bins) {
          if (bin.weight > best.weight) best = bin;
        }

        if (best.weight < 0.05) return resolve(fallbackAccent(key));

        resolve(
          polishAccent(
            Math.round(best.r / best.weight),
            Math.round(best.g / best.weight),
            Math.round(best.b / best.weight),
          ),
        );
      } catch {
        resolve(fallbackAccent(key));
      }
    };
    image.onerror = () => resolve(fallbackAccent(key));
    image.src = url;
  });
}

function Cover({ artwork, title, className = "" }: { artwork?: string | null; title: string; className?: string }) {
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  return artwork && failedUrl !== artwork ? (
    <img
      className={`cover ${className}`}
      src={artwork}
      alt={`${title} 封面`}
      crossOrigin="anonymous"
      loading="lazy"
      decoding="async"
      onError={() => setFailedUrl(artwork)}
    />
  ) : (
    <div className={`cover cover-placeholder ${className}`} aria-label={`${title} 暂无封面`}>
      {initials(title)}
    </div>
  );
}

function App() {
  const [view, setView] = useState<ViewId>(initialView);
  const [viewHistory, setViewHistory] = useState<ViewId[]>([]);
  const [message, setMessageState] = useState("");
  const [messageIsError, setMessageIsError] = useState(false);
  const [snapshot, setSnapshot] = useEngineSnapshot((error) => {
    setMessageState(String(error));
    setMessageIsError(true);
  });
  const {
    alwaysOnTop,
    miniMode,
    sidebarCollapsed,
    setSidebarCollapsed,
    toggleAlwaysOnTop,
    toggleMiniMode,
  } = useWindowPreferences((error) => {
    setMessageState(String(error));
    setMessageIsError(true);
  });
  /** User dismissed the engine error toast; reset when generation/error changes. */
  const [engineErrorDismissed, setEngineErrorDismissed] = useState(false);
  const [accent, setAccent] = useState(FALLBACK_ACCENT);
  const [dragPosition, setDragPosition] = useState<number | null>(null);
  const [volumeDraft, setVolumeDraft] = useState<number | null>(null);
  const [pendingSeek, setPendingSeek] = useState<{ target: number; generation: number; queueKey: string } | null>(null);
  const [outputDevices, setOutputDevices] = useState<string[]>([]);
  const [qualityPreference, setQualityPreference] = useState<QualityPreference>(() => {
    const stored = window.localStorage.getItem("gxplayer.defaultQuality");
    return QUALITY_OPTIONS.some((option) => option.value === stored) ? stored as QualityPreference : "auto";
  });
  const [currentQuality, setCurrentQuality] = useState<string | null>(null);
  const [qualitySwitching, setQualitySwitching] = useState(false);

  const [library, setLibrary] = useState<LibraryTrack[]>([]);
  const [favorites, setFavorites] = useState<LibraryTrack[]>([]);
  const [playlists, setPlaylists] = useState<PlaylistSummary[]>([]);
  const [activePlaylist, setActivePlaylist] = useState<PlaylistSummary | null>(null);
  const [playlistTracks, setPlaylistTracks] = useState<LibraryTrack[]>([]);
  const [newPlaylistName, setNewPlaylistName] = useState("");

  const [sources, setSources] = useState<ListedSource[]>([]);
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [sourceFallback, setSourceFallback] = useState<SourceFallbackConfig>({
    enabled: true,
    sourceIds: [],
    explicitlyConfigured: false,
  });
  const [sourceUrl, setSourceUrl] = useState("");
  const [configSource, setConfigSource] = useState<ListedSource | null>(null);
  const [sourceConfigDraft, setSourceConfigDraft] = useState<SourceConfigDraft | null>(null);
  const [sourceConfigRevealed, setSourceConfigRevealed] = useState(false);
  const [sourceConfigBusy, setSourceConfigBusy] = useState(false);
  const [sourceFallbackBusy, setSourceFallbackBusy] = useState(false);
  const [backupText, setBackupText] = useState("");
  const [cacheStatus, setCacheStatus] = useState<CacheStatus | null>(null);
  const [cacheLimitGiB, setCacheLimitGiB] = useState("5");
  const cacheLimitDirtyRef = useRef(false);
  const [onlineFavorites, setOnlineFavorites] = useState<CatalogTrack[]>([]);
  const [cacheEntries, setCacheEntries] = useState<CacheEntryView[]>([]);
  const [historyEntries, setHistoryEntries] = useState<HistoryEntry[]>([]);
  const [selectedCacheKeys, setSelectedCacheKeys] = useState<string[]>([]);
  const [coverCache, setCoverCache] = useState<Record<string, string>>({});
  const [resolveBanner, setResolveBanner] = useState<{ title: string; detail: string } | null>(null);
  const resolveGenerationRef = useRef(0);
  const resolveAbortRef = useRef(false);
  const activeResolveRequestRef = useRef<string | null>(null);
  const cancelledResolveRequestsRef = useRef<Set<string>>(new Set());
  const suppressNextTerminalAdvanceRef = useRef(false);
  const terminalAdvanceGuardTimerRef = useRef<number | null>(null);
  const searchShellRef = useRef<HTMLDivElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const toastTimerRef = useRef<number | null>(null);

  const [searchQuery, setSearchQuery] = useState("");
  const {
    suggestions,
    suggestionState,
    suggestionError,
    retrySuggestions,
    results: searchResults,
    resultsQuery,
    resultsState,
    resultsError,
    search: searchCatalog,
    retryResults,
    seedResults,
  } = useCatalogSearch(searchQuery);
  const [chartTracks, setChartTracks] = useState<CatalogTrack[]>([]);
  const [suggestionOpen, setSuggestionOpen] = useState(false);
  const [suggestionIndex, setSuggestionIndex] = useState(-1);
  const [playingCatalogKey, setPlayingCatalogKey] = useState<string | null>(null);

  const [selectedCatalogTrack, setSelectedCatalogTrack] = useState<CatalogTrack | null>(null);
  const [lyrics, setLyrics] = useState<LyricDocument | null>(null);
  const lyricsGenerationRef = useRef(0);
  const lyricRefs = useRef<Array<HTMLParagraphElement | null>>([]);

  /** Logical playlist (local paths + online CatalogTrack metadata). Online never pre-resolved. */
  const [playlist, setPlaylist] = useState<PlaylistEntry[]>([]);
  const [playlistIndex, setPlaylistIndex] = useState<number | null>(null);
  const [queuePanelOpen, setQueuePanelOpen] = useState(false);
  const shufflePlayedRef = useRef<Set<number>>(new Set());
  const shuffleRngRef = useRef({ state: (Date.now() ^ 0x9e3779b9) >>> 0 || 1 });
  const advancingRef = useRef(false);
  const playlistRef = useRef(playlist);
  const playlistIndexRef = useRef(playlistIndex);
  const snapshotRef = useRef(snapshot);
  const mediaActionHandlerRef = useRef<(action: TransportAction) => void>(() => undefined);
  const transportCapabilitiesRef = useRef({ signature: "", revision: 0 });
  playlistRef.current = playlist;
  playlistIndexRef.current = playlistIndex;
  snapshotRef.current = snapshot;

  const pushMessage = (text: string, isError = false) => {
    setMessageState(text);
    setMessageIsError(isError);
  };

  const clearMessage = () => {
    setMessageState("");
    setMessageIsError(false);
    setEngineErrorDismissed(true);
  };

  /** Convenience: treat as normal toast unless marked error. */
  const setMessage = (text: string, isError = false) => pushMessage(text, isError);

  const run = async <T,>(command: string, args?: Record<string, unknown>): Promise<T | undefined> => {
    try {
      const result = await invoke<T>(command, args);
      // Don't clear existing toasts on every command — only setMessage callers manage UX.
      return result;
    } catch (error) {
      pushMessage(String(error), true);
      return undefined;
    }
  };

  const refreshLibrary = async () => {
    const [tracks, favoriteTracks, nextPlaylists] = await Promise.all([
      invoke<LibraryTrack[]>("library_tracks"),
      invoke<LibraryTrack[]>("library_favorites"),
      invoke<PlaylistSummary[]>("library_playlists"),
    ]);
    setLibrary(tracks);
    setFavorites(favoriteTracks);
    setPlaylists(nextPlaylists);
  };

  const refreshSources = async () => {
    const [nextSources, nextRuntime, nextFallback] = await Promise.all([
      invoke<ListedSource[]>("source_list"),
      invoke<RuntimeStatus>("source_status"),
      invoke<SourceFallbackConfig>("source_get_fallback_config").catch(() => ({
        enabled: true,
        sourceIds: [],
        explicitlyConfigured: false,
      })),
    ]);
    setSources(nextSources);
    setRuntime(nextRuntime);
    setSourceFallback(nextFallback);
  };

  const refreshCache = async () => {
    const [status, favoriteTracks, entries] = await Promise.all([
      invoke<CacheStatus>("cache_status"),
      invoke<CatalogTrack[]>("cache_online_favorites"),
      invoke<CacheEntryView[]>("cache_list_entries"),
    ]);
    setCacheStatus(status);
    if (!cacheLimitDirtyRef.current) {
      setCacheLimitGiB((status.limitBytes / 1024 / 1024 / 1024).toFixed(2).replace(/\.00$/, ""));
    }
    setOnlineFavorites(favoriteTracks);
    setCacheEntries(entries);
  };

  const refreshHistory = async () => {
    const entries = await invoke<HistoryEntry[]>("library_history", { limit: 100 });
    setHistoryEntries(entries);
  };

  const recordHistory = async (payload: {
    kind: string;
    title: string;
    artist: string;
    path?: string | null;
    providerId?: string | null;
    providerTrackId?: string | null;
    quality?: string | null;
  }) => {
    try {
      await invoke("library_record_history", {
        entry: {
          kind: payload.kind,
          title: payload.title,
          artist: payload.artist,
          path: payload.path ?? null,
          providerId: payload.providerId ?? null,
          providerTrackId: payload.providerTrackId ?? null,
          quality: payload.quality ?? null,
        },
      });
      if (view === "history") void refreshHistory();
    } catch {
      // best-effort
    }
  };

  const cancelResolve = () => {
    const requestId = activeResolveRequestRef.current;
    if (requestId) {
      cancelledResolveRequestsRef.current.add(requestId);
      void invoke("player_cancel_resolve", { requestId }).catch(() => undefined);
    }
    resolveAbortRef.current = true;
    resolveGenerationRef.current += 1;
    activeResolveRequestRef.current = null;
    suppressNextTerminalAdvanceRef.current = true;
    if (terminalAdvanceGuardTimerRef.current) window.clearTimeout(terminalAdvanceGuardTimerRef.current);
    terminalAdvanceGuardTimerRef.current = window.setTimeout(() => {
      suppressNextTerminalAdvanceRef.current = false;
      terminalAdvanceGuardTimerRef.current = null;
    }, 3_000);
    setPlayingCatalogKey(null);
    setResolveBanner(null);
    pushMessage("已取消解析");
  };

  useEffect(() => {
    // Window size is set once in Rust (setup) before first show — do not resize here
    // or the app will open at tauri.conf size then jump larger after React mounts.
    void invoke("ui_ready").catch((error) => setMessage(String(error), true));
    void refreshLibrary().catch((error) => setMessage(String(error), true));
    void refreshSources().catch((error) => setMessage(String(error), true));
    void refreshCache().catch((error) => setMessage(String(error), true));
    void refreshHistory().catch(() => undefined);
    void invoke<string[]>("player_output_devices")
      .then(setOutputDevices)
      .catch((error) => setMessage(String(error), true));
    void invoke<CatalogTrack[]>("metadata_chart", { limit: 12 })
      .then(setChartTracks)
      .catch(() => setChartTracks([]));

    // If the window somehow ended off-screen, recover after first paint.
    void (async () => {
      try {
        const win = getCurrentWindow();
        const pos = await win.outerPosition();
        if (pos.x < -5000 || pos.y < -5000) {
          await invoke("window_force_show");
        }
      } catch {
        // ignore
      }
    })();

    return undefined;
  }, []);

  useEffect(() => {
    if (view === "history") void refreshHistory().catch(() => undefined);
    if (view === "library") {
      void invoke<LibraryTrack[]>("library_scan_missing")
        .then(setLibrary)
        .catch(() => undefined);
    }
  }, [view]);

  useEffect(() => {
    if (view !== "settings" && view !== "library") return;
    void refreshCache().catch((error) => pushMessage(String(error), true));
    const timer = window.setInterval(() => void refreshCache().catch(() => undefined), 2000);
    return () => window.clearInterval(timer);
  }, [view]);

  // Auto-dismiss toasts: normal 3s, error/engine 10s.
  useEffect(() => {
    if (toastTimerRef.current) {
      window.clearTimeout(toastTimerRef.current);
      toastTimerRef.current = null;
    }
    const engineError = snapshot.error && !engineErrorDismissed ? snapshot.error : null;
    const text = engineError || message;
    if (!text) return;
    const isError = Boolean(engineError) || messageIsError;
    toastTimerRef.current = window.setTimeout(() => {
      clearMessage();
    }, isError ? TOAST_ERROR_MS : TOAST_OK_MS);
    return () => {
      if (toastTimerRef.current) {
        window.clearTimeout(toastTimerRef.current);
        toastTimerRef.current = null;
      }
    };
  }, [message, messageIsError, snapshot.error, engineErrorDismissed]);

  // Close search suggestions when clicking outside the search shell.
  useEffect(() => {
    if (!suggestionOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const root = searchShellRef.current;
      if (!root) return;
      if (event.target instanceof Node && !root.contains(event.target)) {
        setSuggestionOpen(false);
        setSuggestionIndex(-1);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [suggestionOpen]);

  // Windows SMTC / taskbar controls share the same frontend-owned transport path.
  useEffect(() => {
    let disposed = false;
    const unlisten = listen<string>("gx-media", (event) => {
      if (disposed) return;
      if (isTransportAction(event.payload)) mediaActionHandlerRef.current(event.payload);
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
    // Handlers use refs for playlist state — safe to bind once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    setEngineErrorDismissed(false);
  }, [snapshot.error, snapshot.generation]);

  const currentQueueItem = useMemo(
    () => (snapshot.queueIndex === null ? null : snapshot.queue[snapshot.queueIndex] ?? null),
    [snapshot.queue, snapshot.queueIndex],
  );
  const currentQueueKey = currentQueueItem ? `${snapshot.queueIndex}:${currentQueueItem.location}` : "";
  const currentLibraryTrack = useMemo(
    () => library.find((track) => track.path === currentQueueItem?.location) ?? null,
    [currentQueueItem?.location, library],
  );
  const currentTitle = selectedCatalogTrack?.title ?? currentLibraryTrack?.title ?? currentQueueItem?.title ?? "尚未播放";
  const currentArtist = selectedCatalogTrack?.artist ?? currentLibraryTrack?.artist ?? "选择一首歌，让房间亮起来";
  const localCover = currentLibraryTrack?.path ? coverCache[currentLibraryTrack.path] ?? null : null;
  const currentArtwork = selectedCatalogTrack?.artworkUrl ?? localCover;

  useEffect(() => {
    const path = currentLibraryTrack?.path;
    if (!path || coverCache[path] || selectedCatalogTrack?.artworkUrl) return;
    let cancelled = false;
    void invoke<{ dataUrl: string } | null>("library_embedded_cover", { path })
      .then((cover) => {
        if (!cancelled && cover?.dataUrl) {
          setCoverCache((prev) => putLruValue(prev, path, cover.dataUrl, COVER_CACHE_LIMIT));
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [currentLibraryTrack?.path, coverCache, selectedCatalogTrack?.artworkUrl]);
  useEffect(() => {
    const path = currentLibraryTrack?.path;
    if (!path) return;
    setCoverCache((prev) => prev[path] ? putLruValue(prev, path, prev[path], COVER_CACHE_LIMIT) : prev);
  }, [currentLibraryTrack?.path]);
  // Loading only while a session is opening — failed must not look like "still playing".
  const isPlaying = snapshot.status === "playing" || snapshot.status === "loading";
  const hasEngineCurrent = currentQueueItem !== null;
  useEffect(() => {
    const useFrontendQueue = playlist.length > 0;
    const hasFrontendCurrent = playlistIndex !== null
      && playlistIndex >= 0
      && playlistIndex < playlist.length;
    const flags = deriveTransportCapabilities({
      queueLength: useFrontendQueue ? playlist.length : snapshot.queue.length,
      currentIndex: useFrontendQueue ? playlistIndex : snapshot.queueIndex,
      // The play action can start the first frontend entry even before an index
      // has been committed to the engine.
      hasCurrent: hasEngineCurrent || hasFrontendCurrent || useFrontendQueue,
      playMode: snapshot.playMode,
    });
    const signature = `${Number(flags.hasCurrent)}:${Number(flags.canPrevious)}:${Number(flags.canNext)}`;
    if (transportCapabilitiesRef.current.signature === signature) return;

    const revision = transportCapabilitiesRef.current.revision + 1;
    transportCapabilitiesRef.current = { signature, revision };
    const capabilities: TransportCapabilities = { revision, ...flags };
    void invoke("player_set_transport_capabilities", { capabilities }).catch((error) => {
      console.warn("[GXPlayer] transport capability sync failed", error);
    });
  }, [
    hasEngineCurrent,
    playlist.length,
    playlistIndex,
    snapshot.playMode,
    snapshot.queue.length,
    snapshot.queueIndex,
  ]);
  const shownPosition = dragPosition ?? pendingSeek?.target ?? snapshot.positionSeconds;
  const shownVolume = volumeDraft ?? snapshot.volume;
  const measuredSourceSpec = formatSourceSpec(snapshot);
  const suspiciousQuality = isSuspiciousQuality(currentQuality, snapshot);
  const selectedOnlineFavorite = selectedCatalogTrack
    ? onlineFavorites.some((track) => track.providerId === selectedCatalogTrack.providerId && track.providerTrackId === selectedCatalogTrack.providerTrackId)
    : false;
  const activeSource = sources.find((source) => source.id === runtime?.activeSourceId || source.active) ?? null;
  const fallbackSources = sourceFallback.sourceIds
    .filter((id) => id !== activeSource?.id)
    .map((id) => sources.find((source) => source.id === id))
    .filter((source): source is ListedSource => Boolean(source));
  const availableFallbackSources = sources.filter(
    (source) => source.id !== activeSource?.id && !fallbackSources.some((fallback) => fallback.id === source.id),
  );
  const sourceStatus = (() => {
    switch (runtime?.state) {
      case "ready":
        return {
          title: "音源已就绪",
          copy: activeSource?.metadata.name ? `当前音源：${activeSource.metadata.name}` : "在线歌曲可解析为整首播放。",
        };
      case "initializing":
        return { title: "音源正在初始化", copy: activeSource?.metadata.name ? `正在启动：${activeSource.metadata.name}` : "请稍候，音源沙箱正在启动。" };
      case "failed":
        return { title: "音源启动失败", copy: runtime.error ?? "请检查音源脚本后重试。" };
      default:
        return { title: "还没有可用音源", copy: "导入 LX 音源脚本后，在线歌曲才能解析为整首播放。" };
    }
  })();

  const navigateTo = (next: ViewId) => {
    if (next === view) return;
    setViewHistory((history) => [...history, view].slice(-32));
    setView(next);
  };

  const navigateBack = () => {
    setViewHistory((history) => {
      const previous = history[history.length - 1];
      if (!previous) return history;
      setView(previous);
      return history.slice(0, -1);
    });
  };

  useEffect(() => {
    if (volumeDraft === null) return;
    if (Math.abs(snapshot.volume - volumeDraft) < 0.005) setVolumeDraft(null);
  }, [snapshot.volume, volumeDraft]);

  useEffect(() => {
    if (!pendingSeek) return;
    if (snapshot.status === "failed" || currentQueueKey !== pendingSeek.queueKey) {
      setPendingSeek(null);
      return;
    }
    if (
      snapshot.generation > pendingSeek.generation
      && Math.abs(snapshot.positionSeconds - pendingSeek.target) < 1.5
    ) {
      setPendingSeek(null);
    }
  }, [currentQueueKey, pendingSeek, snapshot.generation, snapshot.positionSeconds, snapshot.status]);

  useEffect(() => {
    let disposed = false;
    // No artwork → fixed premium default (never muddy hash hues).
    // With artwork → extract; key only used if extraction fails.
    void accentFromArtwork(currentArtwork, `${currentTitle}:${currentArtist}`).then((color) => {
      if (!disposed) setAccent(color);
    });
    return () => {
      disposed = true;
    };
  }, [currentArtwork, currentArtist, currentTitle]);

  useEffect(() => {
    if (!searchQuery.trim()) {
      setSuggestionOpen(false);
      setSuggestionIndex(-1);
      return;
    }
    if (suggestionState !== "idle" && searchShellRef.current?.contains(document.activeElement)) {
      setSuggestionOpen(true);
    }
    setSuggestionIndex(-1);
  }, [searchQuery, suggestionState]);

  const activeLyricIndex = useMemo(() => {
    if (!lyrics) return -1;
    const positionMs = snapshot.positionSeconds * 1000;
    let active = -1;
    lyrics.lines.forEach((line, index) => {
      if (line.timestampMs !== null && line.timestampMs <= positionMs) active = index;
    });
    return active;
  }, [lyrics, snapshot.positionSeconds]);

  useEffect(() => {
    if (activeLyricIndex >= 0) lyricRefs.current[activeLyricIndex]?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeLyricIndex]);

  const artists = useMemo(
    () => [...new Set(suggestions.map((track) => track.artist).filter(Boolean))].slice(0, 2),
    [suggestions],
  );
  const albums = useMemo(
    () => [...new Set(suggestions.map((track) => track.album).filter(Boolean))].slice(0, 2),
    [suggestions],
  );
  const visibleSuggestions = useMemo(() => suggestions.slice(0, 4), [suggestions]);
  const searchOptions = useMemo<SearchOption[]>(() => [
    ...visibleSuggestions.map((track) => ({
      id: `search-track-${encodeURIComponent(catalogKey(track))}`,
      kind: "track" as const,
      track,
    })),
    ...artists.map((artist) => ({
      id: `search-artist-${encodeURIComponent(artist)}`,
      kind: "artist" as const,
      query: artist,
    })),
    ...albums.map((album) => ({
      id: `search-album-${encodeURIComponent(album)}`,
      kind: "album" as const,
      query: album,
    })),
    { id: "search-view-all", kind: "all" as const },
  ], [albums, artists, visibleSuggestions]);

  const clearLyrics = () => {
    lyricsGenerationRef.current += 1;
    setLyrics(null);
  };

  const loadLyricsFor = async (title: string, artist: string, durationMs: number | null, baseMessage: string) => {
    const generation = ++lyricsGenerationRef.current;
    setLyrics(null);
    try {
      const lyricDocument = await invoke<LyricDocument | null>("metadata_lyrics", {
        title,
        artist,
        durationMs,
      });
      if (generation === lyricsGenerationRef.current) setLyrics(lyricDocument);
    } catch (lyricError) {
      if (generation === lyricsGenerationRef.current) {
        setMessage(`${baseMessage} 歌曲已播放，但歌词加载失败：${String(lyricError)}`);
      }
    }
  };

  /**
   * Resolve and play a single online CatalogTrack into the engine.
   * Constraint 2: only called when the playhead actually reaches this track — never batch.
   * Supports cancel + client-side timeout (default 25s).
   */
  const resolveAndPlayOnline = async (
    wanted: CatalogTrack,
    quality: QualityPreference,
    opts?: { allowPreviewFallback?: boolean; candidates?: CatalogTrack[] },
  ): Promise<PlaybackStartResult> => {
    const key = catalogKey(wanted);
    const generation = ++resolveGenerationRef.current;
    const requestId = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${generation}-${Math.random().toString(16).slice(2)}`;
    resolveAbortRef.current = false;
    activeResolveRequestRef.current = requestId;
    suppressNextTerminalAdvanceRef.current = false;
    setResolveBanner({ title: `正在解析《${wanted.title}》`, detail: "可取消 · 超时自动停止 · 仅解析当前这一首" });
    console.info("[GXPlayer] online resolve request", { key, requestId, title: wanted.title, quality });

    const timed = <T,>(promise: Promise<T>, cancelOnTimeout = false): Promise<T> =>
      new Promise<T>((resolve, reject) => {
        const timer = window.setTimeout(() => {
          if (cancelOnTimeout) {
            void invoke("player_cancel_resolve", { requestId }).catch(() => undefined);
          }
          reject(new Error("timeout: 解析超时"));
        }, RESOLVE_TIMEOUT_MS);
        promise.then(
          (value) => {
            window.clearTimeout(timer);
            resolve(value);
          },
          (error) => {
            window.clearTimeout(timer);
            reject(error);
          },
        );
      });

    const interruptedOutcome = (): PlaybackStartResult | null => {
      if (cancelledResolveRequestsRef.current.has(requestId) || resolveAbortRef.current) {
        return { outcome: "cancelled" };
      }
      if (generation !== resolveGenerationRef.current || activeResolveRequestRef.current !== requestId) {
        return { outcome: "stale" };
      }
      return null;
    };

    try {
      const online = await timed(
        invoke<OnlinePlaybackResult>("player_play_online_track", {
          track: wanted,
          quality: quality === "auto" ? null : quality,
          sourceId: null,
          requestId,
        }),
        true,
      );
      const interrupted = interruptedOutcome();
      if (interrupted) return interrupted;
      if (online.outcome === "cancelled" || online.outcome === "stale") {
        return { outcome: online.outcome };
      }
      if (online.outcome === "failed") {
        const diagnostics = formatResolveAttempts(online.attempts);
        throw new Error(`${online.error || "音源未能返回可播放地址"}${diagnostics}`);
      }
      console.info("[GXPlayer] online resolve ok", {
        key,
        cacheHit: online.cacheHit,
        quality: online.quality,
      });
      setSelectedCatalogTrack(online.track);
      setCurrentQuality(online.quality);
      clearLyrics();
      const sourceLabel = online.sourceName || activeSource?.metadata.name || "当前 LX 音源";
      const playbackMessage = online.cacheHit
        ? `已命中本地缓存 · ${online.quality ?? "自动"}，无需再次请求音频直链。`
        : `${sourceLabel} 已解析整首播放${online.quality ? ` · ${online.quality}` : ""}，本次播放会顺手写入缓存。`;
      setMessage(playbackMessage);
      void loadLyricsFor(online.track.title, online.track.artist, online.track.durationMs, playbackMessage);
      void recordHistory({
        kind: "online",
        title: online.track.title,
        artist: online.track.artist,
        providerId: online.track.providerId,
        providerTrackId: online.track.providerTrackId,
        quality: online.quality,
      });
      return STARTED;
    } catch (onlineError) {
      const interrupted = interruptedOutcome();
      if (interrupted) return interrupted;
      console.warn("[GXPlayer] online resolve failed", { key, error: String(onlineError) });
      if (!opts?.allowPreviewFallback) {
        setMessage(formatFailureMessage(onlineError, wanted.title), true);
        return { outcome: "failed", error: onlineError };
      }
      try {
        const preview = await timed(
          invoke<{ track: CatalogTrack; replacedProviderId: string | null }>("metadata_play_preview", {
            wanted,
            candidates: opts.candidates ?? [wanted],
            requestId,
          }),
        );
        const previewInterrupted = interruptedOutcome();
        if (previewInterrupted) return previewInterrupted;
        setSelectedCatalogTrack(preview.track);
        setCurrentQuality("preview");
        clearLyrics();
        const playbackMessage = `LX 整首解析失败，已回退为 ${preview.track.providerId} 官方 30 秒预览。原因：${formatFailureMessage(onlineError)}`;
        setMessage(playbackMessage);
        void loadLyricsFor(preview.track.title, preview.track.artist, preview.track.durationMs, playbackMessage);
        void recordHistory({
          kind: "preview",
          title: preview.track.title,
          artist: preview.track.artist,
          providerId: preview.track.providerId,
          providerTrackId: preview.track.providerTrackId,
          quality: "preview",
        });
        return STARTED;
      } catch (previewError) {
        const previewInterrupted = interruptedOutcome();
        if (previewInterrupted) return previewInterrupted;
        setMessage(formatFailureMessage(`${String(onlineError)}; ${String(previewError)}`, wanted.title), true);
        return { outcome: "failed", error: previewError };
      }
    } finally {
      cancelledResolveRequestsRef.current.delete(requestId);
      if (activeResolveRequestRef.current === requestId) activeResolveRequestRef.current = null;
      if (generation === resolveGenerationRef.current) {
        setResolveBanner(null);
      }
    }
  };

  const playCachedEntry = async (entry: Extract<PlaylistEntry, { kind: "cached" }>) => {
    await invoke("player_play_cache_entry", {
      providerId: entry.providerId,
      providerTrackId: entry.providerTrackId,
      quality: entry.quality,
      title: entry.title,
    });
    setSelectedCatalogTrack(cacheEntryToCatalog({
      providerId: entry.providerId,
      providerTrackId: entry.providerTrackId,
      quality: entry.quality,
      title: entry.title,
      artist: entry.artist,
      album: "",
      byteLen: 0,
      sourceSampleRate: null,
      sourceBitDepth: null,
      sourceChannels: null,
      mediaType: "unknown",
      pinned: false,
      lastAccessedAtMs: 0,
      completedAtMs: 0,
      fileName: "",
    }));
    setCurrentQuality(entry.quality);
    clearLyrics();
    setMessage(`已从本地缓存秒开 · ${entry.quality}`);
  };

  /** Try to start one playlist entry. Does not chain-skip on failure (caller decides). */
  const tryStartEntry = async (
    entries: PlaylistEntry[],
    index: number,
    opts?: { allowPreviewFallback?: boolean },
  ): Promise<PlaybackStartResult> => {
    const entry = entries[index];
    if (!entry) return { outcome: "failed", error: new Error("队列索引无效") };
    suppressNextTerminalAdvanceRef.current = false;
    if (terminalAdvanceGuardTimerRef.current) {
      window.clearTimeout(terminalAdvanceGuardTimerRef.current);
      terminalAdvanceGuardTimerRef.current = null;
    }
    if (entry.kind === "local") {
      try {
        if (playlistIsLocalOnly(entries)) {
          const paths = entries.map((item) => (item as Extract<PlaylistEntry, { kind: "local" }>).path);
          await invoke("player_load_local", { paths, startIndex: index });
        } else {
          await invoke("player_load_local", { paths: [entry.path], startIndex: 0 });
        }
        setSelectedCatalogTrack(null);
        setCurrentQuality(null);
        clearLyrics();
        void recordHistory({ kind: "local", title: entry.title, artist: entry.artist, path: entry.path });
        return STARTED;
      } catch (error) {
        setMessage(formatFailureMessage(error, entry.title), true);
        return { outcome: "failed", error };
      }
    }
    if (entry.kind === "cached") {
      try {
        await playCachedEntry(entry);
        void recordHistory({
          kind: "cached",
          title: entry.title,
          artist: entry.artist,
          providerId: entry.providerId,
          providerTrackId: entry.providerTrackId,
          quality: entry.quality,
        });
        return STARTED;
      } catch (error) {
        setMessage(formatFailureMessage(error, entry.title), true);
        return { outcome: "failed", error };
      }
    }
    const key = catalogKey(entry.track);
    setPlayingCatalogKey(key);
    try {
      return await resolveAndPlayOnline(entry.track, entry.quality, {
        allowPreviewFallback: opts?.allowPreviewFallback,
        candidates: entries
          .filter((item): item is Extract<PlaylistEntry, { kind: "online" }> => item.kind === "online")
          .map((item) => item.track),
      });
    } finally {
      setPlayingCatalogKey(null);
    }
  };

  /**
   * Advance the playhead. On hard failure, skip untried tracks at most once —
   * never infinite-retry under repeat_one / wrap modes.
   * @returns true if a track started successfully.
   */
  const advanceFromIndex = async (
    entries: PlaylistEntry[],
    current: number,
    intent: "ended" | "next" | "previous",
    opts?: { fromFailure?: boolean },
  ): Promise<PlaybackStartResult> => {
    if (advancingRef.current) return { outcome: "stale" };
    advancingRef.current = true;
    const tried = new Set<number>();
    if (opts?.fromFailure) tried.add(current);
    try {
      const mode = snapshotRef.current.playMode ?? "sequential";
      let cursor = current;
      for (let attempt = 0; attempt < Math.max(entries.length, 1); attempt += 1) {
        const next = opts?.fromFailure || attempt > 0
          ? pickFailureSkipIndex(
            mode,
            cursor,
            entries.length,
            tried,
            shufflePlayedRef.current,
            shuffleRngRef.current,
          )
          : frontendNextIndex(
            mode,
            cursor,
            entries.length,
            intent,
            shufflePlayedRef.current,
            shuffleRngRef.current,
          );
        if (next === null) {
          setPlaylistIndex(cursor);
          if (opts?.fromFailure || attempt > 0) {
            setMessage("队列里暂时没有可播放的曲目（解析/加载均失败）。", true);
          }
          return { outcome: "failed" };
        }
        if (tried.has(next)) {
          setMessage("队列里暂时没有可播放的曲目（解析/加载均失败）。", true);
          return { outcome: "failed" };
        }
        tried.add(next);
        setPlaylistIndex(next);
        cursor = next;
        // Failure-skip never uses preview fallback (avoids cascading slow preview attempts).
        const result = await tryStartEntry(entries, next, {
          // Preview fallback only for the user's explicit first click (handled in playPlaylistEntry).
          allowPreviewFallback: false,
        });
        if (result.outcome === "started") return result;
        if (!shouldSkipAfterStart(result)) return result;
        // Subsequent picks in this chain are failure-skips (one pass, no infinite loop).
        opts = { fromFailure: true };
      }
      setMessage("队列里暂时没有可播放的曲目（解析/加载均失败）。", true);
      return { outcome: "failed" };
    } finally {
      advancingRef.current = false;
      setPlayingCatalogKey(null);
    }
  };

  const playPlaylistEntry = async (
    entries: PlaylistEntry[],
    index: number,
    opts?: { allowPreviewFallback?: boolean },
  ): Promise<PlaybackStartResult> => {
    const result = await tryStartEntry(entries, index, opts);
    if (result.outcome === "started" || !shouldSkipAfterStart(result)) return result;
    // First track failed — walk the rest once, then stop (no infinite loop).
    return advanceFromIndex(entries, index, "ended", { fromFailure: true });
  };

  const replacePlaylist = async (
    entries: PlaylistEntry[],
    startIndex: number,
    opts?: { allowPreviewFallback?: boolean },
  ): Promise<PlaybackStartResult> => {
    if (!entries.length) return { outcome: "failed" };
    const index = Math.max(0, Math.min(startIndex, entries.length - 1));
    shufflePlayedRef.current = new Set([index]);
    setPlaylist(entries);
    setPlaylistIndex(index);
    const startKey = entries[index]?.kind === "online" ? catalogKey(entries[index]!.track) : null;
    if (startKey) setPlayingCatalogKey(startKey);
    try {
      return await playPlaylistEntry(entries, index, opts);
    } finally {
      setPlayingCatalogKey(null);
    }
  };

  const chooseFiles = async () => {
    const selected = await open({
      multiple: true,
      directory: false,
      filters: [{ name: "音频", extensions: ["mp3", "flac", "wav", "m4a", "aac", "ogg"] }],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    try {
      const result = await invoke<LibraryImportResult>("library_import_files", { paths });
      await refreshLibrary();
      if (!result.imported.length) {
        const firstFailure = result.failures[0];
        const detail = firstFailure ? `：${firstFailure.error}` : "";
        setMessage(`没有可导入的音频文件${detail}`, true);
        return;
      }

      const acceptedPaths = result.imported.map((track) => track.path);
      await invoke("player_load_local", { paths: acceptedPaths, startIndex: 0 });
      const entries = result.imported.map(localEntryFromLibrary);
      shufflePlayedRef.current = new Set([0]);
      setPlaylist(entries);
      setPlaylistIndex(0);
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      clearLyrics();
      const failureNote = result.failures.length ? `，另有 ${result.failures.length} 个文件导入失败` : "";
      setMessage(`已导入并播放 ${result.imported.length} 首${failureNote}`);
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  /** Click a local track: load the entire current view as the queue, start at the clicked item. */
  const playLocalInList = async (tracks: LibraryTrack[], track: LibraryTrack) => {
    const startIndex = Math.max(0, tracks.findIndex((item) => item.id === track.id));
    const entries = tracks.map(localEntryFromLibrary);
    try {
      await replacePlaylist(entries, startIndex === -1 ? 0 : startIndex);
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const enqueueLocalTracks = async (tracks: LibraryTrack[]) => {
    if (!tracks.length) return;
    const paths = tracks.map((track) => track.path);
    const additions = tracks.map(localEntryFromLibrary);
    const wasEmpty = playlistRef.current.length === 0;
    try {
      await invoke("player_enqueue_local", { paths });
      setPlaylist((prev) => [...prev, ...additions]);
      if (wasEmpty) setPlaylistIndex(0);
      setMessage(`已添加 ${tracks.length} 首到队列`);
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  /** Click a catalog track: queue the whole list as online placeholders; resolve only the clicked one. */
  const playCatalogInList = async (tracks: CatalogTrack[], wanted: CatalogTrack) => {
    if (playingCatalogKey || advancingRef.current) return;
    const list = tracks.length ? tracks : [wanted];
    const startIndex = Math.max(0, list.findIndex((item) => catalogKey(item) === catalogKey(wanted)));
    const entries = list.map((track) => onlineEntryFromCatalog(track, qualityPreference));
    setSuggestionOpen(false);
    // Constraint 2: only the start index is resolved inside replacePlaylist → resolveAndPlayOnline.
    console.info("[GXPlayer] online queue replace", {
      total: entries.length,
      startIndex,
      note: "only the starting track will resolve now; others stay as CatalogTrack metadata",
    });
    try {
      const result = await replacePlaylist(entries, startIndex, { allowPreviewFallback: true });
      // Only leave the current page when something actually began playing.
      if (result.outcome === "started") {
        navigateTo("now-playing");
      }
    } catch (error) {
      setPlayingCatalogKey(null);
      advancingRef.current = false;
      setMessage(`播放失败：${String(error)}`, true);
    }
  };

  const playCatalog = async (wanted: CatalogTrack) => {
    const context =
      searchResults.some((track) => catalogKey(track) === catalogKey(wanted))
        ? searchResults
        : suggestions.some((track) => catalogKey(track) === catalogKey(wanted))
          ? suggestions
          : chartTracks.some((track) => catalogKey(track) === catalogKey(wanted))
            ? chartTracks
            : onlineFavorites.some((track) => catalogKey(track) === catalogKey(wanted))
              ? onlineFavorites
              : [wanted];
    if (playingCatalogKey || advancingRef.current) return;
    await playCatalogInList(context, wanted);
  };

  const enqueueCatalogTracks = (tracks: CatalogTrack[]) => {
    if (!tracks.length) return;
    console.info("[GXPlayer] online enqueue metadata only", { count: tracks.length });
    const wasEmpty = playlistRef.current.length === 0;
    setPlaylist((prev) => [
      ...prev,
      ...tracks.map((track) => onlineEntryFromCatalog(track, qualityPreference)),
    ]);
    if (wasEmpty) setPlaylistIndex(0);
    setMessage(`已添加 ${tracks.length} 首在线歌曲到队列（播放到时再解析）`);
  };

  const cyclePlayMode = async () => {
    const current = snapshot.playMode ?? "sequential";
    const index = PLAY_MODE_ORDER.indexOf(current);
    const next = PLAY_MODE_ORDER[(index + 1) % PLAY_MODE_ORDER.length] ?? "sequential";
    try {
      await invoke("player_set_play_mode", { mode: next });
      // Fresh shuffle cycle when entering shuffle; mark the current track as already heard.
      if (next === "shuffle") {
        shufflePlayedRef.current = new Set(playlistIndex !== null ? [playlistIndex] : []);
        shuffleRngRef.current.state = (Date.now() ^ (playlistIndex ?? 0) ^ 0x9e3779b9) >>> 0 || 1;
      }
      setSnapshot((state) => ({ ...state, playMode: next }));
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const handleTransportNext = async () => {
    const entries = playlistRef.current;
    if (!entries.length) {
      await run("player_next");
      return;
    }
    await advanceFromIndex(entries, playlistIndexRef.current ?? 0, "next");
  };

  const handleTransportPrevious = async () => {
    const entries = playlistRef.current;
    if (!entries.length) {
      await run("player_previous");
      return;
    }
    await advanceFromIndex(entries, playlistIndexRef.current ?? 0, "previous");
  };

  const jumpToPlaylistIndex = async (index: number) => {
    const entries = playlistRef.current;
    const target = entries[index];
    if (!target) return;
    shufflePlayedRef.current.add(index);
    setPlaylistIndex(index);
    if (playlistIsLocalOnly(entries) && target.kind === "local") {
      await run("player_jump", { index });
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      return;
    }
    const key = target.kind === "online" ? catalogKey(target.track) : null;
    if (key) setPlayingCatalogKey(key);
    try {
      await playPlaylistEntry(entries, index);
    } finally {
      if (key) setPlayingCatalogKey(null);
    }
  };

  const playCacheInList = async (entries: CacheEntryView[], wanted: CacheEntryView) => {
    if (playingCatalogKey || advancingRef.current) return;
    const startIndex = Math.max(0, entries.findIndex(
      (item) => item.providerId === wanted.providerId
        && item.providerTrackId === wanted.providerTrackId
        && item.quality === wanted.quality,
    ));
    const playlistEntries = entries.map(cacheEntryToPlaylist);
    try {
      const result = await replacePlaylist(playlistEntries, startIndex === -1 ? 0 : startIndex);
      if (result.outcome === "started") navigateTo("now-playing");
    } catch (error) {
      setPlayingCatalogKey(null);
      advancingRef.current = false;
      setMessage(String(error), true);
    }
  };

  const enqueueCacheEntries = (entries: CacheEntryView[]) => {
    if (!entries.length) return;
    const wasEmpty = playlistRef.current.length === 0;
    setPlaylist((prev) => [...prev, ...entries.map(cacheEntryToPlaylist)]);
    if (wasEmpty) setPlaylistIndex(0);
    setMessage(`已添加 ${entries.length} 首缓存歌曲到队列`);
  };

  const removeCacheEntry = async (entry: CacheEntryView) => {
    if (!window.confirm(`确定删除《${entry.title}》的 ${entry.quality} 缓存吗？`)) return;
    try {
      const status = await invoke<CacheStatus>("cache_remove_entry", {
        providerId: entry.providerId,
        providerTrackId: entry.providerTrackId,
        quality: entry.quality,
      });
      setCacheStatus(status);
      await refreshCache();
      setMessage(`已删除缓存《${entry.title}》· ${entry.quality}`);
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const toggleCachePinned = async (entry: CacheEntryView) => {
    const track = cacheEntryToCatalog(entry);
    // Prefer full catalog metadata from online favorites when available.
    const known = onlineFavorites.find(
      (item) => item.providerId === entry.providerId && item.providerTrackId === entry.providerTrackId,
    ) ?? track;
    try {
      await invoke("cache_set_online_favorite", { track: known, favorite: !entry.pinned });
      await refreshCache();
      setMessage(entry.pinned ? "已取消钉住" : "已收藏并钉住缓存");
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const removePlaylistIndex = async (index: number) => {
    const previous = playlistRef.current;
    const entries = [...previous];
    if (index < 0 || index >= entries.length) return;
    const current = playlistIndexRef.current;
    const removedCurrent = current === index;
    const wasLocalOnly = playlistIsLocalOnly(previous);
    const previousShufflePlayed = new Set(shufflePlayedRef.current);
    if (removedCurrent && activeResolveRequestRef.current) cancelResolve();
    entries.splice(index, 1);
    // Remap shuffle played indices after mid-cycle edits.
    const nextPlayed = new Set<number>();
    shufflePlayedRef.current.forEach((value) => {
      if (value < index) nextPlayed.add(value);
      else if (value > index) nextPlayed.add(value - 1);
    });
    shufflePlayedRef.current = nextPlayed;

    if (wasLocalOnly) {
      try {
        await invoke("player_remove_queue_item", { index });
      } catch (error) {
        shufflePlayedRef.current = previousShufflePlayed;
        setMessage(String(error), true);
        return;
      }
    } else if (!entries.length) {
      try {
        await invoke("player_clear_queue");
      } catch (error) {
        shufflePlayedRef.current = previousShufflePlayed;
        setMessage(String(error), true);
        return;
      }
    }

    setPlaylist(entries);
    if (!entries.length) {
      setPlaylistIndex(null);
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      clearLyrics();
      return;
    }
    let nextIndex: number | null = current;
    if (current === null) nextIndex = null;
    else if (current > index) nextIndex = current - 1;
    else if (current === index) nextIndex = Math.min(index, entries.length - 1);
    setPlaylistIndex(nextIndex);
    // Local-only: engine Remove already reloads when the playing item is deleted.
    // Online/mixed: engine only holds the current resolved track — resolve the replacement.
    if (removedCurrent && nextIndex !== null && !wasLocalOnly) {
      await playPlaylistEntry(entries, nextIndex);
    }
  };

  const clearPlaylist = async () => {
    if (playlistRef.current.length && !window.confirm("确定清空整个播放队列吗？")) return;
    if (activeResolveRequestRef.current) cancelResolve();
    try {
      await invoke("player_clear_queue");
    } catch (error) {
      setMessage(String(error), true);
      return;
    }
    setPlaylist([]);
    setPlaylistIndex(null);
    shufflePlayedRef.current.clear();
    setSelectedCatalogTrack(null);
    setCurrentQuality(null);
    clearLyrics();
    setMessage("队列已清空");
  };

  const reorderPlaylist = async (from: number, to: number) => {
    if (from === to) return;
    const previous = playlistRef.current;
    const previousIndex = playlistIndexRef.current;
    const previousShufflePlayed = new Set(shufflePlayedRef.current);
    const next = moveIndex(previous, from, to);
    let nextIndex = previousIndex;
    if (nextIndex !== null) {
      if (nextIndex === from) nextIndex = to;
      else if (from < nextIndex && to >= nextIndex) nextIndex -= 1;
      else if (from > nextIndex && to <= nextIndex) nextIndex += 1;
    }
    setPlaylist(next);
    setPlaylistIndex(nextIndex);
    shufflePlayedRef.current = new Set();
    // The engine reorders in place; playback position/state must not be reset.
    if (playlistIsLocalOnly(next)) {
      try {
        await invoke("player_reorder_queue", { from, to });
      } catch (error) {
        setPlaylist(previous);
        setPlaylistIndex(previousIndex);
        shufflePlayedRef.current = previousShufflePlayed;
        setMessage(`队列排序失败，已恢复原顺序：${String(error)}`, true);
      }
    }
  };

  const exportBackupFile = async () => {
    const [libraryBackup, sourceBackup] = await Promise.all([
      invoke("library_export_backup"),
      invoke("source_export_backup"),
    ]);
    const text = JSON.stringify({ version: 1, library: libraryBackup, sources: sourceBackup }, null, 2);
    setBackupText(text);
    const path = await save({
      defaultPath: "gxplayer-backup.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    await invoke("backup_write_file", { path, content: text });
    setMessage(`备份已写入 ${path}`);
  };

  const importBackupFile = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    const content = await invoke<string>("backup_read_file", { path });
    setBackupText(content);
    setMessage("已读入备份文件，确认无误后点击「恢复备份」。");
  };

  const removeSelectedCache = async () => {
    if (!selectedCacheKeys.length) return;
    if (!window.confirm(`确定删除选中的 ${selectedCacheKeys.length} 条缓存吗？`)) return;
    const keys = selectedCacheKeys.map((key) => {
      const [providerId, providerTrackId, quality] = key.split("\u0000");
      return { providerId, providerTrackId, quality };
    });
    const status = await invoke<CacheStatus>("cache_remove_entries", { keys });
    setCacheStatus(status);
    setSelectedCacheKeys([]);
    await refreshCache();
    setMessage(`已删除 ${keys.length} 条缓存`);
  };

  const removeCacheByQuality = async (quality: string) => {
    if (!window.confirm(`确定清理所有未钉住的 ${quality} 缓存吗？`)) return;
    const status = await invoke<CacheStatus>("cache_remove_by_quality", {
      quality,
      includePinned: false,
    });
    setCacheStatus(status);
    await refreshCache();
    setMessage(`已清理未钉住的 ${quality} 缓存`);
  };

  // Engine always stops on natural end (no auto-advance). Frontend picks the next index
  // for every non-empty playlist (local, online, mixed) and drives jump / resolve / load.
  // Also recover from engine Failed (decode/stream error) by skipping once — never hang on loading UI.
  const prevStatusRef = useRef(snapshot.status);
  useEffect(() => {
    const prev = prevStatusRef.current;
    prevStatusRef.current = snapshot.status;
    if (snapshot.status === "playing") suppressNextTerminalAdvanceRef.current = false;
    const entries = playlistRef.current;
    if (!entries.length) return;
    const current = playlistIndexRef.current ?? 0;

    if (snapshot.status === "stopped") {
      if (prev === "stopped" || prev === "idle" || prev === "failed") return;
      if (suppressNextTerminalAdvanceRef.current) {
        suppressNextTerminalAdvanceRef.current = false;
        return;
      }
      void advanceFromIndex(entries, current, "ended");
      return;
    }
    if (snapshot.status === "failed" && prev !== "failed") {
      if (suppressNextTerminalAdvanceRef.current) {
        suppressNextTerminalAdvanceRef.current = false;
        return;
      }
      void advanceFromIndex(entries, current, "ended", { fromFailure: true });
    }
  }, [snapshot.status]);

  const switchOnlineQuality = async (preference: QualityPreference) => {
    if (!selectedCatalogTrack || !currentQueueItem?.online || qualitySwitching || resolveBanner) return;
    setQualitySwitching(true);
    const requestId = typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `${Date.now()}-quality`;
    const generation = ++resolveGenerationRef.current;
    resolveAbortRef.current = false;
    activeResolveRequestRef.current = requestId;
    setResolveBanner({ title: `正在切换《${selectedCatalogTrack.title}》的音质`, detail: "可取消 · 当前播放会保留到新音质就绪" });
    const interrupted = () => cancelledResolveRequestsRef.current.has(requestId)
      || resolveAbortRef.current
      || generation !== resolveGenerationRef.current
      || activeResolveRequestRef.current !== requestId;
    try {
      const online = await invoke<OnlinePlaybackResult>("player_play_online_track", {
        track: selectedCatalogTrack,
        quality: preference === "auto" ? null : preference,
        sourceId: null,
        requestId,
      });
      if (interrupted()) return;
      if (online.outcome !== "started") {
        if (online.outcome === "failed") {
          setMessage(`切换音质失败，已保留当前播放：${online.error ?? "没有可播放结果"}`, true);
        }
        return;
      }
      setSelectedCatalogTrack(online.track);
      setCurrentQuality(online.quality);
      setMessage(online.cacheHit ? `已切换到本地缓存 ${online.quality ?? "自动"}。` : `已切换到 ${online.quality ?? "自动"}，并重新开始流式播放。`);
    } catch (error) {
      if (!interrupted()) setMessage(`切换音质失败，已保留当前播放：${String(error)}`, true);
    } finally {
      cancelledResolveRequestsRef.current.delete(requestId);
      if (activeResolveRequestRef.current === requestId) activeResolveRequestRef.current = null;
      if (generation === resolveGenerationRef.current) setResolveBanner(null);
      setQualitySwitching(false);
    }
  };

  const updateQualityPreference = (preference: QualityPreference) => {
    setQualityPreference(preference);
    window.localStorage.setItem("gxplayer.defaultQuality", preference);
  };

  const saveSourceFallback = async (enabled: boolean, sourceIds: string[]) => {
    setSourceFallbackBusy(true);
    try {
      const saved = await invoke<SourceFallbackConfig>("source_set_fallback_config", { enabled, sourceIds });
      setSourceFallback(saved);
      setMessage(enabled ? "自动音源降级顺序已保存。" : "已关闭备用音源自动降级。");
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceFallbackBusy(false);
    }
  };

  const moveFallbackSource = (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (target < 0 || target >= fallbackSources.length) return;
    const next = fallbackSources.map((source) => source.id);
    [next[index], next[target]] = [next[target]!, next[index]!];
    void saveSourceFallback(sourceFallback.enabled, next);
  };

  const addFallbackSource = (sourceId: string) => {
    if (!sourceId) return;
    const next = [...fallbackSources.map((source) => source.id), sourceId];
    void saveSourceFallback(sourceFallback.enabled, next);
  };

  const removeFallbackSource = (sourceId: string) => {
    const next = fallbackSources.map((source) => source.id).filter((id) => id !== sourceId);
    void saveSourceFallback(sourceFallback.enabled, next);
  };

  const openSourceConfig = async (source: ListedSource) => {
    setSourceConfigBusy(true);
    try {
      const config = await invoke<Record<string, unknown>>("source_get_config", { id: source.id });
      const structured = "lsConfig" in config || "keyOverrides" in config;
      const lsConfig = (structured && config.lsConfig && typeof config.lsConfig === "object" && !Array.isArray(config.lsConfig)
        ? config.lsConfig
        : structured ? {} : config) as Record<string, unknown>;
      const keyOverrides = structured && Array.isArray(config.keyOverrides) ? config.keyOverrides : [];
      const firstOverride = keyOverrides.find((item): item is { constName: string; value: string } =>
        Boolean(item && typeof item === "object" && "constName" in item && "value" in item
          && typeof item.constName === "string" && typeof item.value === "string"));
      const api = lsConfig.api && typeof lsConfig.api === "object" && !Array.isArray(lsConfig.api)
        ? lsConfig.api as Record<string, unknown>
        : {};
      setConfigSource(source);
      setSourceConfigDraft({
        lsConfig,
        constName: firstOverride?.constName ?? "YuNingXi",
        keyValue: firstOverride?.value ?? "",
        apiAddr: typeof api.addr === "string" ? api.addr : "",
        apiPass: typeof api.pass === "string" ? api.pass : "",
      });
      setSourceConfigRevealed(false);
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceConfigBusy(false);
    }
  };

  const closeSourceConfig = () => {
    setConfigSource(null);
    setSourceConfigDraft(null);
    setSourceConfigRevealed(false);
  };

  const saveSourceConfig = async () => {
    if (!configSource || !sourceConfigDraft) return;
    setSourceConfigBusy(true);
    try {
      const constName = sourceConfigDraft.constName.trim() || "YuNingXi";
      if (!/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(constName)) throw new Error("常量名不是有效的 JavaScript 标识符");
      const existingApi = sourceConfigDraft.lsConfig.api && typeof sourceConfigDraft.lsConfig.api === "object" && !Array.isArray(sourceConfigDraft.lsConfig.api)
        ? sourceConfigDraft.lsConfig.api as Record<string, unknown>
        : {};
      const config = {
        lsConfig: {
          ...sourceConfigDraft.lsConfig,
          api: { ...existingApi, addr: sourceConfigDraft.apiAddr, pass: sourceConfigDraft.apiPass },
        },
        keyOverrides: sourceConfigDraft.keyValue ? [{ constName, value: sourceConfigDraft.keyValue }] : [],
      };
      await invoke("source_set_config", { id: configSource.id, config });
      closeSourceConfig();
      await refreshSources();
      setMessage(configSource.active ? "音源配置已保存，沙箱已热重载。" : "音源配置已保存，下次启用时生效。");
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceConfigBusy(false);
    }
  };

  const submitSearch = async (queryOverride?: string) => {
    const query = (queryOverride ?? searchQuery).trim();
    if (!query) return;
    if (queryOverride) setSearchQuery(query);
    setSuggestionOpen(false);
    setSuggestionIndex(-1);
    searchInputRef.current?.blur();
    navigateTo("search");
    await searchCatalog(query);
  };

  const activateSearchOption = (option: SearchOption) => {
    setSuggestionOpen(false);
    setSuggestionIndex(-1);
    if (option.kind === "track") {
      void playCatalog(option.track);
      return;
    }
    if (option.kind === "all") {
      void submitSearch();
      return;
    }
    void submitSearch(option.query);
  };

  const onSearchKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      setSuggestionOpen(false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction: 1 | -1 = event.key === "ArrowDown" ? 1 : -1;
      setSuggestionOpen(true);
      setSuggestionIndex((index) => nextOptionIndex(index, searchOptions.length, direction));
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const selected = searchOptions[suggestionIndex];
      if (selected) activateSearchOption(selected);
      else void submitSearch();
    }
  };

  const setAudioMode = async (mode: AudioMode) => {
    try {
      await invoke("player_set_audio_mode", { mode });
      setSnapshot((state) => ({ ...state, audioMode: mode }));
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const toggleFavorite = async (track: LibraryTrack) => {
    await run("library_set_favorite", { trackId: track.id, favorite: !track.favorite });
    await refreshLibrary();
  };

  const toggleOnlineFavorite = async (track: CatalogTrack) => {
    const favorite = !onlineFavorites.some((item) => item.providerId === track.providerId && item.providerTrackId === track.providerTrackId);
    await invoke("cache_set_online_favorite", { track, favorite });
    await refreshCache();
    setMessage(favorite ? "已收藏；现有缓存已钉住，未缓存时会在自然播放完成后自动钉住。" : "已取消收藏；对应缓存恢复为可淘汰状态。");
  };

  const chooseCacheDirectory = async () => {
    const selected = await open({ multiple: false, directory: true });
    if (!selected || Array.isArray(selected)) return;
    const status = await invoke<CacheStatus>("cache_set_directory", { path: selected });
    setCacheStatus(status);
    setMessage("缓存目录已切换；已有缓存不会自动迁移。");
  };

  const saveCacheLimit = async () => {
    const gib = Number(cacheLimitGiB);
    if (!Number.isFinite(gib) || gib <= 0) {
      setMessage("缓存上限必须是正数。", true);
      return;
    }
    const status = await invoke<CacheStatus>("cache_set_limit", { limitBytes: Math.round(gib * 1024 * 1024 * 1024) });
    setCacheStatus(status);
    cacheLimitDirtyRef.current = false;
    setCacheLimitGiB((status.limitBytes / 1024 / 1024 / 1024).toFixed(2).replace(/\.00$/, ""));
    setMessage("缓存上限已保存，超限未收藏条目已按 LRU 清理。");
  };

  const createPlaylist = async () => {
    if (!newPlaylistName.trim()) return;
    const playlist = await run<PlaylistSummary>("library_create_playlist", { name: newPlaylistName.trim() });
    if (playlist) {
      setNewPlaylistName("");
      await refreshLibrary();
      setActivePlaylist(playlist);
      setPlaylistTracks([]);
      navigateTo("playlist");
    }
  };

  const openPlaylist = async (playlist: PlaylistSummary) => {
    const tracks = await run<LibraryTrack[]>("library_playlist_tracks", { playlistId: playlist.id });
    if (tracks) {
      setActivePlaylist(playlist);
      setPlaylistTracks(tracks);
      navigateTo("playlist");
    }
  };

  const addToPlaylist = async (trackId: number, playlistId: number) => {
    await run("library_add_to_playlist", { trackId, playlistId });
    await refreshLibrary();
  };

  const exportBackup = async () => {
    const [libraryBackup, sourceBackup] = await Promise.all([
      invoke("library_export_backup"),
      invoke("source_export_backup"),
    ]);
    setBackupText(JSON.stringify({ version: 1, library: libraryBackup, sources: sourceBackup }, null, 2));
  };

  const restoreBackup = async () => {
    try {
      const backup = JSON.parse(backupText) as { version: number; library: unknown; sources: unknown };
      if (backup.version !== 1) throw new Error("不支持的备份版本");
      await invoke("library_restore_backup", { backup: backup.library });
      await invoke("source_restore_backup", { backup: backup.sources });
      await Promise.all([refreshLibrary(), refreshSources()]);
      setMessage("备份已恢复。");
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const commitSeek = async (seconds: number) => {
    if (!currentQueueItem) return;
    setPendingSeek({ target: seconds, generation: snapshot.generation, queueKey: currentQueueKey });
    setDragPosition(null);
    try {
      await invoke("player_seek", { seconds });
    } catch (error) {
      setPendingSeek(null);
      setMessage(String(error), true);
    }
  };

  const commitVolume = async (volume: number) => {
    setVolumeDraft(volume);
    try {
      await invoke("player_set_volume", { volume });
    } catch (error) {
      setVolumeDraft(null);
      setMessage(String(error), true);
    }
  };

  const handlePlayPause = async () => {
    const currentSnapshot = snapshotRef.current;
    const currentlyPlaying = currentSnapshot.status === "playing" || currentSnapshot.status === "loading";
    if (currentlyPlaying) {
      await run("player_pause");
      return;
    }
    const entries = playlistRef.current;
    const engineItem = currentSnapshot.queueIndex === null
      ? null
      : currentSnapshot.queue[currentSnapshot.queueIndex] ?? null;
    if (!engineItem && entries.length) {
      const index = playlistIndexRef.current ?? 0;
      setPlaylistIndex(index);
      await playPlaylistEntry(entries, index, { allowPreviewFallback: true });
      return;
    }
    await run("player_play");
  };

  mediaActionHandlerRef.current = (action) => {
    switch (action) {
      case "play":
        if (snapshotRef.current.status !== "playing" && snapshotRef.current.status !== "loading") {
          void handlePlayPause();
        }
        break;
      case "pause":
        void run("player_pause");
        break;
      case "toggle":
        void handlePlayPause();
        break;
      case "next":
        void handleTransportNext();
        break;
      case "previous":
        void handleTransportPrevious();
        break;
    }
  };

  const renderTrackRow = (track: LibraryTrack, index: number, list: LibraryTrack[], playlistId?: number) => (
        <div className="track-row" role="listitem" key={track.id}>
          <button className="track-main" onClick={() => void playLocalInList(list, track)} disabled={Boolean(track.missing)}>
            <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span>
              <strong>{track.title}{track.missing ? " · 文件缺失" : ""}</strong>
              <small>
                {track.artist || "未知歌手"}
                {track.album ? ` · ${track.album}` : ""}
                {track.missing ? " · 路径不可用，请重新导入" : ""}
              </small>
            </span>
          </button>
          <time>{formatTime(track.durationSeconds)}</time>
          <button className="icon-button" onClick={() => void enqueueLocalTracks([track])} aria-label="添加到队列" title="添加到队列">＋</button>
          <button className={`icon-button ${track.favorite ? "active" : ""}`} onClick={() => void toggleFavorite(track)} aria-label={track.favorite ? "取消收藏" : "收藏"}>
            {track.favorite ? "♥" : "♡"}
          </button>
          {playlistId ? (
            <button
              className="icon-button"
              aria-label="从歌单移除"
              onClick={async () => {
                await run("library_remove_from_playlist", { playlistId, trackId: track.id });
                await openPlaylist(activePlaylist!);
                await refreshLibrary();
              }}
            >
              ×
            </button>
          ) : (
            <select aria-label={`将 ${track.title} 添加到歌单`} defaultValue="" onChange={(event) => {
              const playlist = Number(event.target.value);
              if (playlist) void addToPlaylist(track.id, playlist);
              event.target.value = "";
            }}>
              <option value="">＋ 歌单</option>
              {playlists.map((item) => <option value={item.id} key={item.id}>{item.name}</option>)}
            </select>
          )}
        </div>
  );

  const renderTrackRows = (tracks: LibraryTrack[], playlistId?: number) =>
    tracks.length > 120 ? (
      <VirtualTrackList tracks={tracks} renderRow={(track, index) => renderTrackRow(track, index, tracks, playlistId)} />
    ) : (
      <div className="track-list" role="list">{tracks.map((track, index) => renderTrackRow(track, index, tracks, playlistId))}</div>
    );

  const renderCatalogRows = (tracks: CatalogTrack[]) => (
    <div className="catalog-grid">
      {tracks.map((track) => {
        const trackKey = catalogKey(track);
        const resolving = playingCatalogKey === trackKey;
        return (
        <div className="catalog-card-wrap" key={trackKey}>
          <button className="catalog-card" disabled={resolving} aria-busy={resolving} onClick={() => void playCatalogInList(tracks, track)}>
            <Cover artwork={track.artworkUrl} title={track.title} />
            <strong>{track.title}</strong>
            <span>{track.artist}</span>
            <small>{resolving ? "正在解析整首播放…" : track.album || track.providerId}</small>
            <i aria-hidden="true">{resolving ? "…" : "▶"}</i>
          </button>
          <button
            type="button"
            className="catalog-enqueue"
            onClick={() => enqueueCatalogTracks([track])}
            aria-label={`将 ${track.title} 添加到队列`}
            title="添加到队列（播放到时再解析）"
          >
            ＋ 队列
          </button>
        </div>
      )})}
    </div>
  );

  const renderCacheRows = (entries: CacheEntryView[]) => (
    <div className="track-list cache-track-list" role="list">
      {entries.map((entry, index) => {
        const rowKey = `${entry.providerId}\u0000${entry.providerTrackId}\u0000${entry.quality}`;
        const selected = selectedCacheKeys.includes(rowKey);
        return (
        <div className="track-row cache-row" role="listitem" key={rowKey}>
          <label className="cache-select">
            <input
              type="checkbox"
              checked={selected}
              onChange={(event) => {
                setSelectedCacheKeys((prev) =>
                  event.target.checked ? [...prev, rowKey] : prev.filter((key) => key !== rowKey),
                );
              }}
              aria-label={`选择 ${entry.title}`}
            />
          </label>
          <button className="track-main" type="button" onClick={() => void playCacheInList(entries, entry)}>
            <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span>
              <strong>{entry.title}</strong>
              <small>
                {entry.artist || "未知歌手"}
                {entry.album ? ` · ${entry.album}` : ""}
                {" · "}
                {entry.quality}
                {" · "}
                {formatBytes(entry.byteLen)}
                {entry.pinned ? " · 已钉住" : ""}
              </small>
            </span>
          </button>
          <span className="cache-quality-badge" title="音质档位">{entry.quality}</span>
          <time title="缓存大小">{formatBytes(entry.byteLen)}</time>
          <button
            type="button"
            className="icon-button"
            onClick={() => enqueueCacheEntries([entry])}
            aria-label="添加到队列"
            title="添加到队列"
          >
            ＋
          </button>
          <button
            type="button"
            className={`icon-button ${entry.pinned ? "active" : ""}`}
            onClick={() => void toggleCachePinned(entry)}
            aria-label={entry.pinned ? "取消钉住" : "收藏钉住"}
            title={entry.pinned ? "取消钉住" : "收藏并钉住"}
          >
            {entry.pinned ? "♥" : "♡"}
          </button>
          <button
            type="button"
            className="icon-button"
            onClick={() => void removeCacheEntry(entry)}
            aria-label="删除缓存"
            title="删除此缓存"
          >
            ×
          </button>
        </div>
        );
      })}
    </div>
  );

  const displayPlaylist = playlist;
  const displayIndex = playlistIndex;
  const upNext = displayPlaylist.length && displayIndex !== null
    ? displayPlaylist.slice(displayIndex + 1, displayIndex + 6)
    : [];

  const renderView = () => {
    if (view === "discovery") return (
      <div className="page discovery-page">
        <section className="hero-panel panel-enter">
          <div>
            <p className="eyebrow">GXPLAYER · YOUR ROOM, YOUR SOUND</p>
            <h1><span>让音乐留在</span><span>原本的位置。</span></h1>
            <p>默认原声直通。需要电影和游戏的空间感时，再打开影院/游戏模式。</p>
            <div className="hero-actions">
              <button className="primary" onClick={chooseFiles}>导入本地音乐</button>
              <button onClick={() => navigateTo("now-playing")}>打开播放页</button>
            </div>
          </div>
          <div
            className={`mini-stage ${snapshot.audioMode === "music" ? "bypassed" : "enabled"}`}
            aria-label={`当前音效模式：${snapshot.audioMode === "music" ? "原声音乐" : "影院游戏"}`}
          >
            <div className="stage-glow" aria-hidden="true" />
            <div className="stage-orbit stage-orbit-outer" />
            <div className="stage-orbit stage-orbit-inner" />
            <span className="stage-listener">你</span>
            <i className="speaker speaker-left" />
            <i className="speaker speaker-right" />
            <strong className="stage-badge">{snapshot.audioMode === "music" ? "原声" : "空间"}</strong>
          </div>
        </section>
        <section className="section-block panel-enter delay-1">
          <div className="section-heading"><div><p className="eyebrow">RECENTLY ADDED</p><h2>最近加入</h2></div><button onClick={() => navigateTo("library")}>查看曲库 →</button></div>
          {library.length ? renderTrackRows(library.slice(0, 6)) : <EmptyState title="曲库还是空的" copy="导入熟悉的音乐，从原声模式开始。" action="选择音乐" onAction={chooseFiles} />}
        </section>
        <section className="playlist-strip panel-enter delay-2">
          <div className="section-heading"><div><p className="eyebrow">PLAYLISTS</p><h2>你的歌单</h2></div></div>
          <div className="playlist-cards">
            {playlists.map((playlist) => <button className="playlist-card" key={playlist.id} onClick={() => void openPlaylist(playlist)}><span>♫</span><strong>{playlist.name}</strong><small>{playlist.trackCount} 首</small></button>)}
            <label className="playlist-card create-card"><span>＋</span><input aria-label="新歌单名称" placeholder="新歌单" value={newPlaylistName} onChange={(event) => setNewPlaylistName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void createPlaylist(); }} /><button onClick={() => void createPlaylist()} disabled={!newPlaylistName.trim()}>创建</button></label>
          </div>
        </section>
        {chartTracks.length > 0 && <section className="section-block panel-enter delay-2"><div className="section-heading"><div><p className="eyebrow">DISCOVER</p><h2>正在流行</h2></div><button onClick={() => { seedResults(chartTracks, "中国区热门"); setSearchQuery("中国区热门"); navigateTo("search"); }}>查看全部 →</button></div>{renderCatalogRows(chartTracks.slice(0, 6))}</section>}
      </div>
    );

    if (view === "search") return (
      <div className="page">
        <PageHeading eyebrow="SEARCH" title={resultsQuery ? `“${resultsQuery}” 的结果` : "搜索音乐"} copy={runtime?.state === "ready" ? `${sourceStatus.copy} 点击歌曲将优先解析整首播放，失败时会明确提示并回退官方 30 秒预览。` : `${sourceStatus.title}：${sourceStatus.copy} 当前仍可尝试官方 30 秒预览。`} />
        {resultsState === "loading" ? (
          <LoadingState />
        ) : resultsState === "error" ? (
          <ErrorState title="搜索没有完成" copy={resultsError ?? "请检查网络或音源后重试。"} onRetry={retryResults} />
        ) : searchResults.length ? (
          renderCatalogRows(searchResults)
        ) : resultsState === "empty" ? (
          <EmptyState title="没有找到相关音乐" copy="换一个歌名、歌手或专辑关键词试试。" />
        ) : (
          <EmptyState title="从顶栏开始搜索" copy="输入歌名、歌手或专辑，联想结果会按类型分组。" />
        )}
      </div>
    );

    if (view === "library") {
      return (
        <div className="page">
          <PageHeading
            eyebrow="LIBRARY"
            title="曲库"
            copy={`${library.length} 首本地导入 · ${cacheEntries.length} 首在线缓存。本地文件与在线缓存分开展示。`}
            action={<button className="primary" onClick={chooseFiles}>导入音乐</button>}
          />
          <section className="section-block">
            <div className="section-heading">
              <div>
                <h3>本地导入</h3>
                <p>你从磁盘选择并导入的音频文件。</p>
              </div>
            </div>
            {library.length
              ? renderTrackRows(library)
              : <EmptyState title="还没有本地导入" copy="选择音频文件导入，或先播放在线歌曲生成下方缓存。" action="选择音乐" onAction={chooseFiles} />}
          </section>
          <section className="section-block">
            <div className="section-heading">
              <div>
                <h3>在线缓存</h3>
                <p>完整播放在线歌曲后写入；不会预下载。收藏会钉住缓存。</p>
              </div>
              {cacheEntries.length > 0 ? (
                <div className="cache-bulk-actions">
                  <button type="button" className="primary" onClick={() => enqueueCacheEntries(cacheEntries)}>全部入队</button>
                  <button type="button" disabled={!selectedCacheKeys.length} onClick={() => void removeSelectedCache()}>
                    删除所选 ({selectedCacheKeys.length})
                  </button>
                  <select
                    aria-label="按音质清理"
                    defaultValue=""
                    onChange={(event) => {
                      const quality = event.target.value;
                      if (quality) void removeCacheByQuality(quality);
                      event.target.value = "";
                    }}
                  >
                    <option value="">按音质清理…</option>
                    {["flac24bit", "flac", "320k", "128k"].map((q) => (
                      <option key={q} value={q}>{q}</option>
                    ))}
                  </select>
                </div>
              ) : null}
            </div>
            <div className="tip-banner" role="note">
              <strong>缓存说明</strong>
              <span>
                {cacheStatus
                  ? `当前 ${cacheStatus.entryCount} 项 · ${formatBytes(cacheStatus.totalBytes)} · 钉住 ${cacheStatus.pinnedCount}。只保存自然播放收到的字节。`
                  : "只保存自然播放时已收到的字节，不会预抓或批量下载。"}
              </span>
            </div>
            {cacheEntries.length
              ? renderCacheRows(cacheEntries)
              : <EmptyState title="还没有在线缓存" copy="完整播放一首在线歌曲后会出现在这里，可秒开离线听。" />}
          </section>
        </div>
      );
    }

    if (view === "favorites") {
      const tracks = favorites;
      return (
        <div className="page">
          <PageHeading
            eyebrow="FAVORITES"
            title="我的收藏"
            copy={`${tracks.length + onlineFavorites.length} 首收藏；在线收藏的缓存会被钉住。`}
          />
          {onlineFavorites.length > 0 && (
            <section className="section-block">
              <div className="section-heading"><div><h3>在线收藏</h3><p>尚未缓存的歌曲不会主动下载，会等你自然播放。</p></div></div>
              {renderCatalogRows(onlineFavorites)}
            </section>
          )}
          {tracks.length ? (
            <section className="section-block">
              <div className="section-heading"><div><h3>本地收藏</h3></div></div>
              {renderTrackRows(tracks)}
            </section>
          ) : onlineFavorites.length === 0 ? (
            <EmptyState title="还没有收藏" copy="播放在线歌曲或打开曲库，点一下心形即可收藏。" />
          ) : null}
        </div>
      );
    }

    if (view === "history") {
      return (
        <div className="page">
          <PageHeading
            eyebrow="HISTORY"
            title="播放历史"
            copy={`${historyEntries.length} 条最近播放（最多保留 500）。`}
            action={<button type="button" className="danger" onClick={async () => { if (!window.confirm("确定清空全部播放历史吗？")) return; try { await invoke("library_clear_history"); await refreshHistory(); } catch (error) { setMessage(String(error), true); } }}>清空历史</button>}
          />
          {historyEntries.length === 0 ? (
            <EmptyState title="还没有播放记录" copy="听歌后会出现在这里，方便找回昨晚那首。" />
          ) : (
            <div className="track-list" role="list">
              {historyEntries.map((entry) => (
                <div className="track-row" role="listitem" key={entry.id}>
                  <button
                    type="button"
                    className="track-main"
                    onClick={() => {
                      if (entry.kind === "local" && entry.path) {
                        void playLocalInList(
                          [{ id: -1, path: entry.path, title: entry.title, artist: entry.artist, album: "", durationSeconds: null, favorite: false, addedAtMs: 0 }],
                          { id: -1, path: entry.path, title: entry.title, artist: entry.artist, album: "", durationSeconds: null, favorite: false, addedAtMs: 0 },
                        );
                      } else if (entry.providerId && entry.providerTrackId) {
                        void playCatalog({
                          providerId: entry.providerId,
                          providerTrackId: entry.providerTrackId,
                          title: entry.title,
                          artist: entry.artist,
                          album: "",
                          durationMs: null,
                          artworkUrl: null,
                          resolverPayload: {},
                          preview: null,
                        });
                      }
                    }}
                  >
                    <span className="track-index">{entry.kind.slice(0, 2)}</span>
                    <span>
                      <strong>{entry.title}</strong>
                      <small>{entry.artist || "未知歌手"} · {new Date(entry.playedAtMs).toLocaleString()}</small>
                    </span>
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      );
    }

    if (view === "playlist") return (
      <div className="page"><PageHeading eyebrow="PLAYLIST" title={activePlaylist?.name ?? "歌单"} copy={`${playlistTracks.length} 首音乐`} action={activePlaylist ? <button className="danger" onClick={async () => { if (!window.confirm(`确定删除歌单“${activePlaylist.name}”吗？`)) return; try { await invoke("library_delete_playlist", { playlistId: activePlaylist.id }); navigateTo("discovery"); setActivePlaylist(null); await refreshLibrary(); } catch (error) { setMessage(String(error), true); } }}>删除歌单</button> : undefined} />{playlistTracks.length && activePlaylist ? renderTrackRows(playlistTracks, activePlaylist.id) : <EmptyState title="这个歌单还没有歌" copy="回到曲库，把想听的歌加进来。" action="去曲库" onAction={() => navigateTo("library")} />}</div>
    );

    if (view === "sources") return (
      <div className="page"><PageHeading eyebrow="MUSIC SOURCES" title="管理音源" copy="音源脚本运行在独立沙箱中；程序启动时也会自动扫描 %APPDATA%\\com.gxplayer.desktop\\sources\\drop-in 里的 .js。" action={<button onClick={async () => { const selected = await open({ multiple: false, filters: [{ name: "LX 音源脚本", extensions: ["js"] }] }); if (selected && !Array.isArray(selected)) { try { await invoke("source_import_file", { path: selected }); await refreshSources(); } catch (error) { setMessage(String(error), true); } } }}>导入脚本</button>} />
        <SourceGuide />
        <section className="source-status-card"><span className={`runtime-dot ${runtime?.state ?? "no_source"}`} /><div><strong>{sourceStatus.title}</strong><p>{sourceStatus.copy}</p></div><code>GEN {runtime?.generation ?? 0}</code></section>
        <section className="source-fallback-card" aria-labelledby="source-fallback-title">
          <div className="source-fallback-heading">
            <div><p className="eyebrow">FALLBACK</p><h3 id="source-fallback-title">自动降级</h3><p>主音源失败时按顺序尝试备用音源；每个音源内部仍会继续做音质降级。</p></div>
            <label className="source-fallback-toggle"><input type="checkbox" checked={sourceFallback.enabled} disabled={sourceFallbackBusy} onChange={(event) => void saveSourceFallback(event.target.checked, fallbackSources.map((source) => source.id))} /> 启用</label>
          </div>
          <div className="fallback-main"><span>主音源</span><strong>{activeSource?.metadata.name || "尚未启用音源"}</strong></div>
          {fallbackSources.length ? (
            <ol className="fallback-list">
              {fallbackSources.map((source, index) => <li key={source.id}><span>{index + 1}</span><div><strong>{source.metadata.name || "未命名音源"}</strong><small>{source.metadata.author || source.id}</small></div><div className="fallback-actions"><button type="button" disabled={sourceFallbackBusy || index === 0} onClick={() => moveFallbackSource(index, -1)} aria-label={`上移 ${source.metadata.name}`}>↑</button><button type="button" disabled={sourceFallbackBusy || index === fallbackSources.length - 1} onClick={() => moveFallbackSource(index, 1)} aria-label={`下移 ${source.metadata.name}`}>↓</button><button type="button" disabled={sourceFallbackBusy} onClick={() => removeFallbackSource(source.id)}>移除</button></div></li>)}
            </ol>
          ) : <div className="fallback-empty">还没有备用音源。主源会继续逐档降低音质，最终可回退到官方 30 秒预览。</div>}
          {availableFallbackSources.length > 0 && <select aria-label="添加备用音源" defaultValue="" disabled={sourceFallbackBusy} onChange={(event) => { addFallbackSource(event.target.value); event.target.value = ""; }}><option value="">＋ 添加备用音源…</option>{availableFallbackSources.map((source) => <option key={source.id} value={source.id}>{source.metadata.name || source.id}</option>)}</select>}
        </section>
        <div className="inline-form"><input aria-label="音源脚本 URL" placeholder="https://…/source.js" value={sourceUrl} onChange={(event) => setSourceUrl(event.target.value)} /><button className="primary" disabled={!sourceUrl.trim()} onClick={async () => { try { await invoke("source_import_url", { url: sourceUrl.trim() }); setSourceUrl(""); await refreshSources(); } catch (error) { setMessage(String(error), true); } }}>从 URL 导入</button></div>
        <div className="source-list">{sources.map((source) => <article className={`source-card ${source.active ? "active" : ""}`} key={source.id}><div><span className="source-badge">{source.active ? "正在使用" : source.hasConfig ? "已配置" : "可用"}</span><h3>{source.metadata.name || "未命名音源"}</h3><p>{source.metadata.author || "未知作者"} · v{source.metadata.version || "?"}</p></div><div className="source-actions"><label><input type="checkbox" checked={source.updatesEnabled} onChange={async (event) => { try { await invoke("source_set_updates_enabled", { id: source.id, enabled: event.target.checked }); await refreshSources(); } catch (error) { setMessage(String(error), true); } }} /> 更新提醒</label><button disabled={sourceConfigBusy} onClick={() => void openSourceConfig(source)}>配置</button><button disabled={source.active} onClick={async () => { try { await invoke("source_activate", { id: source.id }); await refreshSources(); } catch (error) { setMessage(String(error), true); } }}>启用</button><button className="danger" onClick={async () => { if (!window.confirm(`确定删除音源“${source.metadata.name || source.id}”吗？`)) return; try { await invoke("source_remove", { id: source.id }); await refreshSources(); } catch (error) { setMessage(String(error), true); } }}>删除</button></div></article>)}</div>
      </div>
    );

    if (view === "settings") return (
      <div className="page"><PageHeading eyebrow="SETTINGS" title="设置与备份" copy="输出设备、窗口和本地数据都在这里管理。" />
        <div className="settings-grid">
          <section className="settings-card"><h3>输出设备</h3><p>切换时会从当前位置继续播放。</p><select value={snapshot.outputDevice ?? ""} onChange={(event) => void run("player_set_output_device", { name: event.target.value || null })}><option value="">系统默认设备</option>{outputDevices.map((device) => <option key={device} value={device}>{device}</option>)}</select></section>
          <section className="settings-card"><h3>默认音质</h3><p>自动会按当前平台能力从高到低尝试，并在解析失败时逐档回退。</p><select value={qualityPreference} onChange={(event) => updateQualityPreference(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></section>
          <section className="settings-card"><h3>默认听感</h3><p>音乐模式保持 DSP 透明旁路；影院/游戏模式启用空间处理。</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></section>
          <section className="settings-card">
            <h3>窗口</h3>
            <p>位置与尺寸会自动记忆；迷你模式适合边听边干活。</p>
            <div className="cache-actions">
              <button type="button" className={alwaysOnTop ? "primary" : ""} onClick={() => void toggleAlwaysOnTop()}>{alwaysOnTop ? "取消置顶" : "窗口置顶"}</button>
              <button type="button" className={miniMode ? "primary" : ""} onClick={() => void toggleMiniMode()}>{miniMode ? "退出迷你" : "迷你模式"}</button>
            </div>
          </section>
          <section className="settings-card cache-settings"><h3>在线播放缓存</h3><p>只保存自然播放时已经收到的字节，不会预抓或批量下载。批量管理请到「曲库」页的在线缓存分区。</p><dl><div><dt>当前占用</dt><dd>{cacheStatus ? `${formatBytes(cacheStatus.totalBytes)} · ${cacheStatus.entryCount} 项` : "读取中…"}</dd></div><div><dt>收藏钉住</dt><dd>{cacheStatus?.pinnedCount ?? 0} 项</dd></div><div><dt>目录</dt><dd title={cacheStatus?.directory}>{cacheStatus?.directory ?? "读取中…"}</dd></div></dl><label><span>上限（GiB）</span><div className="inline-form"><input type="number" min="0.125" step="0.5" value={cacheLimitGiB} onChange={(event) => { cacheLimitDirtyRef.current = true; setCacheLimitGiB(event.target.value); }} /><button onClick={() => void saveCacheLimit()}>保存</button></div></label><div className="cache-actions"><button onClick={() => void chooseCacheDirectory()}>选择目录</button><button onClick={async () => { const status = await invoke<CacheStatus>("cache_reset_directory"); setCacheStatus(status); setMessage("已恢复默认缓存目录；旧目录内容未迁移。"); }}>恢复默认</button><button onClick={async () => { if (!window.confirm("确定清理所有未收藏缓存吗？")) return; const status = await invoke<CacheStatus>("cache_clear", { includePinned: false }); setCacheStatus(status); }}>清未收藏</button><button className="danger" onClick={async () => { if (!window.confirm("确定清空全部缓存（包括收藏钉住项）吗？")) return; const status = await invoke<CacheStatus>("cache_clear", { includePinned: true }); setCacheStatus(status); }}>清空全部</button></div></section>
        </div>
        <section className="backup-card">
          <div className="section-heading">
            <div>
              <h3>配置备份</h3>
              <p>包含本地曲库、歌单、音源脚本及音源密钥；可存为文件或从文件读入。备份内容请勿公开。</p>
            </div>
            <div className="cache-actions">
              <button type="button" onClick={() => void exportBackup()}>生成到文本框</button>
              <button type="button" onClick={() => void exportBackupFile()}>存为文件…</button>
              <button type="button" onClick={() => void importBackupFile()}>从文件读入…</button>
              <button type="button" className="primary" disabled={!backupText.trim()} onClick={() => void restoreBackup()}>恢复备份</button>
            </div>
          </div>
          <textarea aria-label="GXPlayer 备份 JSON" placeholder="生成的备份会显示在这里，也可以粘贴已有备份。" value={backupText} onChange={(event) => setBackupText(event.target.value)} />
        </section>
      </div>
    );

    return (
      <div className="page now-playing-page">
        <div className="now-grid">
          <section className={`record-column ${isPlaying ? "is-playing" : ""}`}>
            <div className={`record-stage ${isPlaying ? "live" : ""}`}>
              <div className="record-glow" aria-hidden="true" />
              <div className={`record ${isPlaying ? "spinning" : ""}`}>
                <Cover artwork={currentArtwork} title={currentTitle} className="record-cover" />
                <span className="record-hole" />
              </div>
              <div className={`eq-bars ${isPlaying ? "active" : ""}`} aria-hidden="true">
                <i /><i /><i /><i /><i />
              </div>
            </div>
            <p className="eyebrow">NOW PLAYING</p>
            <h1 className={isPlaying ? "title-live" : ""}>{currentTitle}</h1>
            <p className="artist-line">{currentArtist}</p>
            {measuredSourceSpec && (
              <p className={`source-spec ${suspiciousQuality ? "suspicious" : ""}`}>
                {currentQuality && currentQueueItem?.online ? <><span>{currentQuality}（自报）</span><b>·</b></> : null}
                <span>实测 {measuredSourceSpec}</span>
                {suspiciousQuality && <em title="自报高解析音质与解码规格不一致">⚠ 疑似虚标</em>}
              </p>
            )}
          </section>
          <section className="stage-panel">
            <div className={`sound-stage ${snapshot.audioMode === "music" ? "bypassed" : "enabled"}`} aria-label="声场模式盘">
              <div className="stage-field" aria-hidden="true" />
              <div className="stage-ring stage-ring-outer" aria-hidden="true" />
              <div className="orbit orbit-one" />
              <div className="orbit orbit-two" />
              <div className="stage-ring stage-ring-core" aria-hidden="true" />
              <span className="listener">你</span>
              <i className="stage-speaker front-left"><b>FL</b></i>
              <i className="stage-speaker front-right"><b>FR</b></i>
              <i className="stage-speaker rear-left"><b>RL</b></i>
              <i className="stage-speaker rear-right"><b>RR</b></i>
              <span className="stage-mode-chip">{snapshot.audioMode === "music" ? "直通" : "空间"}</span>
            </div>
            <div className="mode-copy">
              <p className="eyebrow">SOUND MODE</p>
              <h2>{snapshot.audioMode === "music" ? "原声 / 音乐" : "影院 / 游戏"}</h2>
              <p>{snapshot.audioMode === "music" ? "透明直通，不添加空间处理。你的盲测首选。" : "Crossfeed + 立体声 HRTF，仅在需要空间感时开启。"}</p>
              <ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} />
            </div>
          </section>
        </div>
        <section className="lyrics-panel"><div className="lyrics-scroll">{lyrics?.instrumental ? <p className="lyric active">纯音乐</p> : lyrics?.lines.length ? lyrics.lines.map((line, index) => <p className={`lyric ${index === activeLyricIndex ? "active" : ""}`} key={`${line.timestampMs}-${index}`} ref={(element) => { lyricRefs.current[index] = element; }}>{line.text}</p>) : <div className="lyrics-empty"><strong>歌词会出现在这里</strong><span>在线预览会自动匹配同步歌词。</span></div>}</div></section>
        {upNext.length > 0 && (
          <section className="up-next-panel panel-enter">
            <div className="section-heading">
              <div>
                <p className="eyebrow">UP NEXT</p>
                <h3>接下来播放</h3>
              </div>
              <button type="button" onClick={() => setQueuePanelOpen(true)}>打开队列</button>
            </div>
            <ul className="up-next-list">
              {upNext.map((entry, offset) => {
                const absolute = (displayIndex ?? 0) + 1 + offset;
                return (
                  <li key={entryKey(entry, absolute)}>
                    <button type="button" onClick={() => void jumpToPlaylistIndex(absolute)}>
                      <span>{String(absolute + 1).padStart(2, "0")}</span>
                      <strong>{entryTitle(entry)}</strong>
                      <small>{entryArtist(entry)}{entry.kind === "online" ? " · 待解析" : entry.kind === "cached" ? " · 缓存" : ""}</small>
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        )}
      </div>
    );
  };

  return (
    <div className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""} ${miniMode ? "mini-mode" : ""}`} style={{ "--accent": accent } as CSSProperties}>
      <div className="ambient-light" aria-hidden="true" />
      <div className="ambient-light ambient-light-secondary" aria-hidden="true" />
      <div className="shell-noise" aria-hidden="true" />
      <header className="top-bar" data-tauri-drag-region>
        <div className="brand-cluster">
          <button className="menu-button" onClick={() => setSidebarCollapsed((value) => !value)} aria-pressed={!sidebarCollapsed} aria-label={sidebarCollapsed ? "展开侧栏" : "收起侧栏"}>☰</button>
          <button className="logo" onClick={() => navigateTo("discovery")} aria-label="返回探索页"><img src={gxplayerIcon} alt="" /></button>
          <button className="history-back" onClick={navigateBack} disabled={!viewHistory.length} aria-label="返回上一页" title="返回上一页">‹</button>
          <button className="mini-mode-exit" type="button" onClick={() => void toggleMiniMode()}>退出迷你</button>
        </div>
        <div className="global-search" ref={searchShellRef}>
          <span aria-hidden="true">⌕</span>
          <input
            ref={searchInputRef}
            role="combobox"
            aria-label="搜索歌曲、歌手、专辑"
            aria-autocomplete="list"
            aria-expanded={suggestionOpen}
            aria-controls="search-suggestions"
            aria-activedescendant={suggestionIndex >= 0 ? searchOptions[suggestionIndex]?.id : undefined}
            placeholder="搜索歌曲、歌手、专辑…"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            onFocus={() => searchQuery.trim() && setSuggestionOpen(true)}
            onKeyDown={onSearchKeyDown}
          />
          {suggestionState === "loading" && <i className="search-spinner" aria-label="正在搜索联想" />}
          {suggestionOpen && (
            <div className="suggestions" id="search-suggestions" role="listbox" aria-label="搜索联想">
              {suggestionState === "loading" && <div className="suggestion-state">正在查找联想…</div>}
              {suggestionState === "empty" && <div className="suggestion-state">没有找到相关音乐</div>}
              {suggestionState === "error" && (
                <div className="suggestion-state suggestion-error">
                  <span>{suggestionError ?? "联想加载失败"}</span>
                  <button type="button" onClick={retrySuggestions}>重试</button>
                </div>
              )}
              {visibleSuggestions.length > 0 && (
                <SuggestionGroup label="歌曲">
                  {visibleSuggestions.map((track) => {
                    const trackKey = `${track.providerId}:${track.providerTrackId}`;
                    const resolving = playingCatalogKey === trackKey;
                    const optionIndex = searchOptions.findIndex((option) => option.kind === "track" && catalogKey(option.track) === trackKey);
                    const option = searchOptions[optionIndex];
                    return (
                      <button
                        role="option"
                        id={option?.id}
                        aria-selected={optionIndex === suggestionIndex}
                        aria-busy={resolving}
                        disabled={resolving}
                        className={optionIndex === suggestionIndex ? "selected" : ""}
                        key={trackKey}
                        onMouseDown={(event) => event.preventDefault()}
                        onMouseEnter={() => setSuggestionIndex(optionIndex)}
                        onClick={() => option && activateSearchOption(option)}
                      >
                        <span>{resolving ? "…" : "♪"}</span>
                        <strong>{track.title}</strong>
                        <small>{resolving ? "正在解析整首播放…" : track.artist}</small>
                      </button>
                    );
                  })}
                </SuggestionGroup>
              )}
              {artists.length > 0 && (
                <SuggestionGroup label="歌手">
                  {artists.map((artist) => {
                    const optionIndex = searchOptions.findIndex((option) => option.kind === "artist" && option.query === artist);
                    const option = searchOptions[optionIndex];
                    return <button
                      role="option"
                      id={option?.id}
                      aria-selected={optionIndex === suggestionIndex}
                      className={optionIndex === suggestionIndex ? "selected" : ""}
                      key={artist}
                      onMouseDown={(event) => event.preventDefault()}
                      onMouseEnter={() => setSuggestionIndex(optionIndex)}
                      onClick={() => option && activateSearchOption(option)}
                    >
                      <span>●</span>
                      <strong>{artist}</strong>
                      <small>歌手</small>
                    </button>;
                  })}
                </SuggestionGroup>
              )}
              {albums.length > 0 && (
                <SuggestionGroup label="专辑">
                  {albums.map((album) => {
                    const optionIndex = searchOptions.findIndex((option) => option.kind === "album" && option.query === album);
                    const option = searchOptions[optionIndex];
                    return <button
                      role="option"
                      id={option?.id}
                      aria-selected={optionIndex === suggestionIndex}
                      className={optionIndex === suggestionIndex ? "selected" : ""}
                      key={album}
                      onMouseDown={(event) => event.preventDefault()}
                      onMouseEnter={() => setSuggestionIndex(optionIndex)}
                      onClick={() => option && activateSearchOption(option)}
                    >
                      <span>◉</span>
                      <strong>{album}</strong>
                      <small>专辑</small>
                    </button>;
                  })}
                </SuggestionGroup>
              )}
              <button
                id="search-view-all"
                role="option"
                aria-selected={searchOptions[suggestionIndex]?.kind === "all"}
                className={`view-all ${searchOptions[suggestionIndex]?.kind === "all" ? "selected" : ""}`}
                onMouseDown={(event) => event.preventDefault()}
                onMouseEnter={() => setSuggestionIndex(searchOptions.length - 1)}
                onClick={() => activateSearchOption({ id: "search-view-all", kind: "all" })}
              >
                查看“{searchQuery}”的全部结果 <span>→</span>
              </button>
            </div>
          )}
        </div>
        <div className="top-bar-trail">
          <button className={`mode-pill ${snapshot.audioMode === "cinema_game" ? "active" : ""}`} onClick={() => navigateTo("now-playing")}><span>⊙</span>{snapshot.audioMode === "music" ? "原声" : "空间"}</button>
        </div>
        <div className="window-controls"><button onClick={() => void getCurrentWindow().minimize()} aria-label="最小化">─</button><button className="maximize-control" onClick={() => void getCurrentWindow().toggleMaximize()} aria-label="最大化">□</button><button className="close" onClick={() => void getCurrentWindow().close()} aria-label="关闭">×</button></div>
      </header>

      <aside className="sidebar">
        <nav>{NAV_ITEMS.map((item) => <button className={view === item.id ? "active" : ""} onClick={() => navigateTo(item.id)} key={item.id} title={item.label}><span>{item.icon}</span><strong>{item.label}</strong></button>)}</nav>
        <div className="sidebar-playlists"><p>歌单</p>{playlists.slice(0, 8).map((playlist) => <button key={playlist.id} className={activePlaylist?.id === playlist.id && view === "playlist" ? "active" : ""} onClick={() => void openPlaylist(playlist)} title={playlist.name}><span>♬</span><strong>{playlist.name}</strong></button>)}</div>
        <div className="engine-health"><i className={snapshot.status === "failed" ? "bad" : ""} /><span><strong>Rust Engine</strong><small>{snapshot.status === "failed" ? "需要处理" : `${snapshot.underrunCallbacks} underrun`}</small></span></div>
      </aside>

      <main className="content">{renderView()}</main>

      {configSource && sourceConfigDraft && <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeSourceConfig(); }}><section className="config-modal" role="dialog" aria-modal="true" aria-label={`${configSource.metadata.name} 音源配置`}><div className="section-heading"><div><p className="eyebrow">SOURCE CONFIG</p><h3>{configSource.metadata.name || "音源配置"}</h3><p>同时支持源码常量 key 与 LX 全局 ls；关闭或保存后敏感值会从界面状态清空。</p></div><button onClick={closeSourceConfig} aria-label="关闭配置">×</button></div><div className="config-fields"><label><span>源码常量名</span><input value={sourceConfigDraft.constName} placeholder="YuNingXi" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, constName: event.target.value })} /></label><label><span>解析 Key</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.keyValue} placeholder="留空则使用音源公益额度" autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, keyValue: event.target.value })} /></label><label><span>ls.api.addr（可选）</span><input value={sourceConfigDraft.apiAddr} placeholder="https://…" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiAddr: event.target.value })} /></label><label><span>ls.api.pass（可选）</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.apiPass} autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiPass: event.target.value })} /></label></div><label className="config-reveal"><input type="checkbox" checked={sourceConfigRevealed} onChange={(event) => setSourceConfigRevealed(event.target.checked)} /> 临时显示敏感字段</label><div className="modal-actions"><button onClick={closeSourceConfig}>取消</button><button className="primary" disabled={sourceConfigBusy} onClick={() => void saveSourceConfig()}>保存并应用</button></div></section></div>}

      {(message || (snapshot.error && !engineErrorDismissed)) && (
        <div
          className={`toast ${snapshot.error && !engineErrorDismissed || messageIsError ? "toast-error" : "toast-ok"}`}
          role={snapshot.error && !engineErrorDismissed || messageIsError ? "alert" : "status"}
        >
          <span>{snapshot.error && !engineErrorDismissed || messageIsError ? "!" : "✓"}</span>
          <p>{(!engineErrorDismissed && snapshot.error) ? snapshot.error : message}</p>
          <button type="button" onClick={clearMessage} aria-label="关闭提示">×</button>
        </div>
      )}

      <footer className="player-bar">
        <button className={`player-track ${isPlaying ? "is-playing" : ""}`} onClick={() => navigateTo("now-playing")}>
          <span className={`player-cover-wrap ${isPlaying ? "live" : ""}`}>
            <Cover artwork={currentArtwork} title={currentTitle} />
            {isPlaying && <span className="player-eq" aria-hidden="true"><i /><i /><i /></span>}
          </span>
          <span>
            <strong>{currentTitle}</strong>
            <small>{currentArtist}</small>
          </span>
        </button>
        <div className="player-center">
          <div className="transport">
            <button
              type="button"
              className="transport-btn"
              onClick={() => void cyclePlayMode()}
              aria-label={PLAY_MODE_META[snapshot.playMode ?? "sequential"].label}
              title={PLAY_MODE_META[snapshot.playMode ?? "sequential"].label}
            >
              <span className={`glyph-mode glyph-mode-${PLAY_MODE_META[snapshot.playMode ?? "sequential"].glyph}`} aria-hidden="true" />
            </button>
            <button type="button" className="transport-btn" onClick={() => void handleTransportPrevious()} aria-label="上一首">
              <span className="glyph-prev" aria-hidden="true" />
            </button>
            <button type="button" className="play-button" onClick={() => void handlePlayPause()} disabled={!currentQueueItem && !displayPlaylist.length} aria-label={isPlaying ? "暂停" : "播放"}>
              <span className={isPlaying ? "glyph-pause" : "glyph-play"} aria-hidden="true" />
            </button>
            <button type="button" className="transport-btn" onClick={() => void handleTransportNext()} aria-label="下一首">
              <span className="glyph-next" aria-hidden="true" />
            </button>
          </div>
          <div className="timeline">
            <time>{formatTime(shownPosition)}</time>
            <input
              aria-label="播放进度"
              type="range"
              className="seek-slider"
              min={0}
              max={Math.max(snapshot.durationSeconds ?? 0, 0.01)}
              step={0.05}
              value={Math.min(shownPosition, Math.max(snapshot.durationSeconds ?? 0, 0.01))}
              disabled={!currentQueueItem || !snapshot.durationSeconds}
              style={
                {
                  "--fill": `${snapshot.durationSeconds ? (Math.min(shownPosition, snapshot.durationSeconds) / snapshot.durationSeconds) * 100 : 0}%`,
                } as CSSProperties
              }
              onChange={(event) => setDragPosition(Number(event.target.value))}
              onPointerUp={(event) => void commitSeek(Number(event.currentTarget.value))}
              onKeyUp={(event) => {
                if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) void commitSeek(Number(event.currentTarget.value));
              }}
            />
            <time>{formatTime(snapshot.durationSeconds)}</time>
          </div>
        </div>
        <div className="player-tools">
          {selectedCatalogTrack && currentQueueItem?.online && <button className={`online-favorite ${selectedOnlineFavorite ? "active" : ""}`} onClick={() => void toggleOnlineFavorite(selectedCatalogTrack)} aria-label={selectedOnlineFavorite ? "取消在线收藏" : "收藏在线歌曲"} title={selectedOnlineFavorite ? "取消收藏" : "收藏并钉住缓存"}>{selectedOnlineFavorite ? "♥" : "♡"}</button>}
          {measuredSourceSpec && <span className={`measured-quality ${suspiciousQuality ? "suspicious" : ""}`} title={`${currentQuality ? `${currentQuality}（音源自报） · ` : ""}实测 ${measuredSourceSpec}${suspiciousQuality ? " · 疑似虚标" : ""}`}>{suspiciousQuality ? "⚠ " : ""}{measuredSourceSpec}</span>}
          {selectedCatalogTrack && currentQueueItem?.online && <select className="quality-select" aria-label="音源自报音质" title={`音源自报档位：${currentQuality ?? "自动"}`} value={QUALITY_OPTIONS.some((option) => option.value === currentQuality) ? currentQuality ?? "auto" : "auto"} disabled={qualitySwitching || Boolean(resolveBanner)} onChange={(event) => void switchOnlineQuality(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.value === "auto" ? `自动${currentQuality ? ` · ${currentQuality}` : ""}` : option.label}</option>)}</select>}
          <div className="volume-cluster">
            <span className="volume-icon" aria-hidden="true" />
            <input
              aria-label="音量"
              type="range"
              className="volume-slider"
              min={0}
              max={1}
              step={0.01}
              value={shownVolume}
              style={{ "--fill": `${shownVolume * 100}%` } as CSSProperties}
              onChange={(event) => setVolumeDraft(Number(event.target.value))}
              onPointerUp={(event) => {
                const volume = Number(event.currentTarget.value);
                void commitVolume(volume);
              }}
              onKeyUp={(event) => {
                if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) {
                  void commitVolume(Number(event.currentTarget.value));
                }
              }}
              onBlur={(event) => {
                if (volumeDraft !== null) void commitVolume(Number(event.currentTarget.value));
              }}
            />
          </div>
          <button
            type="button"
            className={`tool-btn ${snapshot.audioMode === "cinema_game" ? "active" : ""}`}
            onClick={() => void setAudioMode(snapshot.audioMode === "music" ? "cinema_game" : "music")}
            aria-label="切换音效模式"
            title={snapshot.audioMode === "music" ? "原声直通" : "影院/游戏空间"}
          >
            <span className="glyph-spatial" aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`tool-btn ${queuePanelOpen ? "active" : ""}`}
            onClick={() => setQueuePanelOpen((open) => !open)}
            aria-label="播放队列"
            title={`播放队列${displayPlaylist.length ? ` · ${displayPlaylist.length}` : ""}`}
          >
            <span className="glyph-queue" aria-hidden="true" />
          </button>
          <button type="button" className="tool-btn more-btn" onClick={() => navigateTo("settings")} aria-label="更多设置" title="设置与备份">
            <span className="more-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
          </button>
        </div>
      </footer>

      <QueuePanel
        open={queuePanelOpen}
        playMode={snapshot.playMode ?? "sequential"}
        rows={displayPlaylist.map((entry, index) => ({
          key: entryKey(entry, index),
          title: entryTitle(entry),
          subtitle: `${entryArtist(entry)} · ${entrySourceLabel(entry)}${entry.kind === "online" && index !== displayIndex ? " · 待解析" : ""}`,
          active: index === displayIndex,
        }))}
        onClose={() => setQueuePanelOpen(false)}
        onClear={() => void clearPlaylist()}
        onJump={(index) => void jumpToPlaylistIndex(index)}
        onRemove={(index) => void removePlaylistIndex(index)}
        onReorder={(from, to) => void reorderPlaylist(from, to)}
      />

      <ResolveBanner
        visible={Boolean(resolveBanner)}
        title={resolveBanner?.title ?? "正在解析"}
        detail={resolveBanner?.detail}
        onCancel={cancelResolve}
      />
    </div>
  );
}

function PageHeading({ eyebrow, title, copy, action }: { eyebrow: string; title: string; copy: string; action?: ReactNode }) {
  return <header className="page-heading panel-enter"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{copy}</p></div>{action}</header>;
}

function EmptyState({ title, copy, action, onAction }: { title: string; copy: string; action?: string; onAction?: () => void }) {
  return <div className="empty-state"><span>♫</span><h3>{title}</h3><p>{copy}</p>{action && <button className="primary" onClick={onAction}>{action}</button>}</div>;
}

function ErrorState({ title, copy, onRetry }: { title: string; copy: string; onRetry: () => void }) {
  return <div className="empty-state error-state" role="alert"><span>!</span><h3>{title}</h3><p>{copy}</p><button className="primary" type="button" onClick={onRetry}>重试</button></div>;
}

function LoadingState() {
  return <div className="empty-state"><i className="large-spinner" /><h3>正在找音乐</h3><p>搜索会同时整理不同平台的结果。</p></div>;
}

function ModeButtons({ mode, onChange }: { mode: AudioMode; onChange: (mode: AudioMode) => Promise<void> }) {
  return (
    <div className="mode-buttons" role="radiogroup" aria-label="音效模式">
      <button role="radio" aria-checked={mode === "music"} className={mode === "music" ? "active" : ""} onClick={() => void onChange("music")}>
        <span>♫</span>
        <strong>原声 / 音乐</strong>
        <small>默认 · 透明直通</small>
      </button>
      <button role="radio" aria-checked={mode === "cinema_game"} className={mode === "cinema_game" ? "active" : ""} onClick={() => void onChange("cinema_game")}>
        <span>◎</span>
        <strong>影院 / 游戏</strong>
        <small>可选 · 空间处理</small>
      </button>
    </div>
  );
}

function SuggestionGroup({ label, children }: { label: string; children: ReactNode }) {
  return <section className="suggestion-group"><p>{label}</p>{children}</section>;
}

function VirtualTrackList({ tracks, renderRow }: { tracks: LibraryTrack[]; renderRow: (track: LibraryTrack, index: number) => ReactNode }) {
  const rowHeight = 68;
  const viewportHeight = 544;
  const [scrollTop, setScrollTop] = useState(0);
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - 4);
  const visibleCount = Math.ceil(viewportHeight / rowHeight) + 8;
  const end = Math.min(tracks.length, start + visibleCount);
  return <div className="track-list virtual-track-list" role="list" style={{ height: viewportHeight }} onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}><div className="virtual-track-space" style={{ height: tracks.length * rowHeight }}><div className="virtual-track-window" style={{ transform: `translateY(${start * rowHeight}px)` }}>{tracks.slice(start, end).map((track, offset) => renderRow(track, start + offset))}</div></div></div>;
}

export default App;
