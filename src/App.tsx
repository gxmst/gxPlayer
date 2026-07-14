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
import { QueuePanel, type QueueAvailabilityStatus } from "./components/QueuePanel";
import { ResolveBanner } from "./components/ResolveBanner";
import { TextPlaylistImportDialog } from "./components/TextPlaylistImportDialog";
import { isRemoteArtworkUrl, useArtworkUrl } from "./hooks/useArtwork";
import { useBackupRestore } from "./hooks/useBackupRestore";
import { useCatalogSearch } from "./hooks/useCatalogSearch";
import { useEngineSnapshot } from "./hooks/useEngineSnapshot";
import { useLiveVolume } from "./hooks/useLiveVolume";
import { useNarrowLayout } from "./hooks/useNarrowLayout";
import { useSystemProxySettings } from "./hooks/useSystemProxySettings";
import { useWindowActivity } from "./hooks/useWindowActivity";
import { useWindowPreferences } from "./hooks/useWindowPreferences";
import {
  frontendNextIndex,
  moveIndex,
  pickFailureSkipIndex,
} from "./lib/playlistLogic";
import { splitArtistNames } from "./lib/artistNames";
import { diagnosticEntryDisplay } from "./lib/diagnosticDisplay";
import { groupConsecutiveHistory } from "./lib/historyGrouping";
import {
  engineMatchesLocalQueue,
  localQueuePaths,
  relinkLocalQueuePath,
  unavailablePathsFromChecks,
  type LocalPathAvailability,
} from "./lib/localQueueAvailability";
import {
  loadPlaylistSession,
  savePlaylistSession,
  type PersistablePlaylistEntry,
  type QualityPreference,
} from "./lib/playlistPersistence";
import { formatFailureMessage } from "./lib/resolveErrors";
import {
  STARTED,
  nextOptionIndex,
  putLruValue,
  shouldSkipAfterStart,
  type PlaybackStartResult,
} from "./lib/uiState";
import {
  loadThemePreference,
  saveThemePreference,
  THEME_OPTIONS,
  type ThemeId,
} from "./lib/themePreference";
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
  type DiagnosticLogEntry,
  type DiagnosticLogExportResult,
  type DiagnosticLogStatus,
  type EngineSnapshot,
  type HistoryEntry,
  type LibraryImportResult,
  type LibraryPlaylistItem,
  type LibraryTrack,
  type ListedSource,
  type LyricDocument,
  type OnlinePlaybackResult,
  type PlayMode,
  type PlaylistSummary,
  type PreviewCacheStatus,
  type ResolveAttemptDiagnostic,
  type RuntimeStatus,
  type ViewId,
} from "./types";

type AudioMode = EngineSnapshot["audioMode"];
type CloseBehavior = "hide_to_tray" | "exit";
type AppPreferences = {
  version: number;
  closeBehavior: CloseBehavior;
  closeToTrayNoticeShown: boolean;
  volume: number;
  outputDevice: string | null;
};
type OutputDeviceStatus = {
  devices: string[];
  defaultDevice: string | null;
  selectedDevice: string | null;
};
type OutputDeviceFallbackEvent = {
  unavailableDevice: string;
  fallbackDevice: string | null;
};
type SourceConfigDraft = {
  json: string;
  enabled: boolean;
  updatesEnabled: boolean;
};
type SearchOption =
  | { id: string; kind: "track"; track: CatalogTrack }
  | { id: string; kind: "artist" | "album"; query: string }
  | { id: string; kind: "all" };

/** Frontend playlist entry. Online items store metadata only, never resolved URLs. */
type PlaylistEntry = PersistablePlaylistEntry;

const PLAY_MODE_ORDER: PlayMode[] = ["sequential", "repeat_all", "repeat_one", "shuffle"];
const PLAY_MODE_META: Record<PlayMode, { label: string; glyph: string }> = {
  sequential: { label: "顺序播放", glyph: "seq" },
  repeat_all: { label: "列表循环", glyph: "all" },
  repeat_one: { label: "单曲循环", glyph: "one" },
  shuffle: { label: "随机播放", glyph: "shuf" },
};
const TOAST_OK_MS = 3_000;
const TOAST_ERROR_MS = 10_000;
const MAX_CONSECUTIVE_FAILURE_SKIPS = 3;
const COVER_CACHE_LIMIT = 96;
let lyricsRequestSequence = 0;

function nextLyricsRequestId(): string {
  lyricsRequestSequence += 1;
  return `lyrics-${Date.now()}-${lyricsRequestSequence}`;
}

function isMetadataCancellation(error: unknown): boolean {
  const message = String(error).toLowerCase();
  return message.includes("cancel") || message.includes("取消");
}

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

function cachedIdentityKey(providerId: string, providerTrackId: string, quality: string): string {
  return `${providerId}\u0000${providerTrackId}\u0000${quality}`;
}

function libraryPlaylistItemToQueueEntry(item: LibraryPlaylistItem): PlaylistEntry {
  if (item.kind === "local") return localEntryFromLibrary(item.track);
  return {
    kind: "cached",
    providerId: item.providerId,
    providerTrackId: item.providerTrackId,
    quality: item.quality,
    title: item.title,
    artist: item.artist,
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

function localEntryFromLibrary(track: LibraryTrack): Extract<PlaylistEntry, { kind: "local" }> {
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
  return requested && ["discovery", "search", "artist", "library", "history", "favorites", "playlist", "sources", "settings", "now-playing"].includes(requested)
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

function Cover({ artwork, title, className = "", eager = false }: { artwork?: string | null; title: string; className?: string; eager?: boolean }) {
  const placeholderRef = useRef<HTMLDivElement | null>(null);
  const remote = isRemoteArtworkUrl(artwork);
  const [visibleArtwork, setVisibleArtwork] = useState<string | null>(() => remote && eager ? artwork : null);
  const resolvedArtwork = useArtworkUrl(artwork, !remote || eager || visibleArtwork === artwork);
  const [failedUrl, setFailedUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!remote || eager) return;
    const target = placeholderRef.current;
    if (!target || typeof IntersectionObserver === "undefined") {
      setVisibleArtwork(artwork);
      return;
    }
    setVisibleArtwork(null);
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setVisibleArtwork(artwork);
        observer.disconnect();
      }
    }, { rootMargin: "160px" });
    observer.observe(target);
    return () => observer.disconnect();
  }, [artwork, eager, remote]);

  return resolvedArtwork && failedUrl !== resolvedArtwork ? (
    <img
      className={`cover ${className}`}
      src={resolvedArtwork}
      alt={`${title} 封面`}
      loading="lazy"
      decoding="async"
      onError={() => setFailedUrl(resolvedArtwork)}
    />
  ) : (
    <div ref={placeholderRef} className={`cover cover-placeholder ${className}`} aria-label={`${title} 暂无封面`}>
      {initials(title)}
    </div>
  );
}

function ArtistLinks({ artist, onSelect, className = "", fallback = "未知歌手" }: {
  artist: string;
  onSelect: (artist: string) => void;
  className?: string;
  fallback?: string;
}) {
  const names = splitArtistNames(artist);
  if (!names.length) return <span className={className}>{fallback}</span>;
  return (
    <span className={`artist-links ${className}`.trim()}>
      {names.map((name, index) => (
        <span className="artist-credit" key={name}>
          {index > 0 && <span className="artist-separator" aria-hidden="true">、</span>}
          <span
            className="artist-link"
            role="link"
            tabIndex={0}
            aria-label={`查看歌手 ${name}`}
            onClick={(event) => {
              event.stopPropagation();
              onSelect(name);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                event.stopPropagation();
                onSelect(name);
              }
            }}
          >{name}</span>
        </span>
      ))}
    </span>
  );
}

function App() {
  const windowActive = useWindowActivity();
  const isNarrow = useNarrowLayout();
  const [restoredPlaylistSession] = useState(loadPlaylistSession);
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
    isMaximized,
    sidebarCollapsed,
    setSidebarCollapsed,
    toggleAlwaysOnTop,
    toggleMiniMode,
  } = useWindowPreferences((error) => {
    setMessageState(String(error));
    setMessageIsError(true);
  });
  const narrowLayout = isNarrow && !miniMode;
  const [sidebarDrawerOpen, setSidebarDrawerOpen] = useState(false);
  const menuButtonRef = useRef<HTMLButtonElement>(null);
  const sidebarRef = useRef<HTMLElement>(null);
  const {
    status: proxyStatus,
    busy: proxyBusy,
    refresh: refreshProxyStatus,
    setMode: setProxyMode,
  } = useSystemProxySettings((error) => {
    setMessageState(String(error));
    setMessageIsError(true);
  });
  /** User dismissed the engine error toast; reset when generation/error changes. */
  const [engineErrorDismissed, setEngineErrorDismissed] = useState(false);
  const [accent, setAccent] = useState(FALLBACK_ACCENT);
  const [theme, setTheme] = useState<ThemeId>(() => loadThemePreference());
  const [themePickerOpen, setThemePickerOpen] = useState(false);
  const [dragPosition, setDragPosition] = useState<number | null>(null);
  const [pendingSeek, setPendingSeek] = useState<{ target: number; generation: number; queueKey: string } | null>(null);
  const [outputDevices, setOutputDevices] = useState<string[]>([]);
  const [outputDeviceStatus, setOutputDeviceStatus] = useState<OutputDeviceStatus | null>(null);
  const [outputDeviceBusy, setOutputDeviceBusy] = useState(false);
  const [appPreferences, setAppPreferences] = useState<AppPreferences | null>(null);
  const [closeNoticeOpen, setCloseNoticeOpen] = useState(false);
  const [closeNoticeBusy, setCloseNoticeBusy] = useState(false);
  const [outputDeviceFallback, setOutputDeviceFallback] = useState<OutputDeviceFallbackEvent | null>(null);
  const closeNoticeConfirmRef = useRef<HTMLButtonElement>(null);
  const [qualityPreference, setQualityPreference] = useState<QualityPreference>(() => {
    const stored = window.localStorage.getItem("gxplayer.defaultQuality");
    return QUALITY_OPTIONS.some((option) => option.value === stored) ? stored as QualityPreference : "auto";
  });
  const [currentQuality, setCurrentQuality] = useState<string | null>(null);
  const [qualitySwitching, setQualitySwitching] = useState(false);
  const [textPlaylistDialogOpen, setTextPlaylistDialogOpen] = useState(false);

  const [library, setLibrary] = useState<LibraryTrack[]>([]);
  const [favorites, setFavorites] = useState<LibraryTrack[]>([]);
  const [playlists, setPlaylists] = useState<PlaylistSummary[]>([]);
  const [activePlaylist, setActivePlaylist] = useState<PlaylistSummary | null>(null);
  const [playlistItems, setPlaylistItems] = useState<LibraryPlaylistItem[]>([]);
  const [newPlaylistName, setNewPlaylistName] = useState("");

  const [sources, setSources] = useState<ListedSource[]>([]);
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [draggedSource, setDraggedSource] = useState<string | null>(null);
  const [sourceOrderBusy, setSourceOrderBusy] = useState(false);
  const [sourceActionBusy, setSourceActionBusy] = useState<{ id: string; kind: "toggle" | "reimport" | "remove" } | null>(null);
  const [sourceUrl, setSourceUrl] = useState("");
  const [sourceImportBusy, setSourceImportBusy] = useState<"file" | "url" | null>(null);
  const [configSource, setConfigSource] = useState<ListedSource | null>(null);
  const [sourceConfigDraft, setSourceConfigDraft] = useState<SourceConfigDraft | null>(null);
  const [sourceConfigRevealed, setSourceConfigRevealed] = useState(false);
  const [sourceConfigBusy, setSourceConfigBusy] = useState(false);
  const [backupText, setBackupText] = useState("");
  const [diagnosticLogStatus, setDiagnosticLogStatus] = useState<DiagnosticLogStatus | null>(null);
  const [diagnosticLogEntries, setDiagnosticLogEntries] = useState<DiagnosticLogEntry[]>([]);
  const [diagnosticLogBusy, setDiagnosticLogBusy] = useState<"refresh" | "toggle" | "export" | "clear" | null>(null);
  const diagnosticLogGenerationRef = useRef(0);
  const [cacheStatus, setCacheStatus] = useState<CacheStatus | null>(null);
  const [previewCacheStatus, setPreviewCacheStatus] = useState<PreviewCacheStatus | null>(null);
  const [cacheLimitGiB, setCacheLimitGiB] = useState("5");
  const cacheLimitDirtyRef = useRef(false);
  const [onlineFavorites, setOnlineFavorites] = useState<CatalogTrack[]>([]);
  const [cacheEntries, setCacheEntries] = useState<CacheEntryView[]>([]);
  const availableCacheKeys = useMemo(
    () => new Set(cacheEntries.map((entry) => (
      cachedIdentityKey(entry.providerId, entry.providerTrackId, entry.quality)
    ))),
    [cacheEntries],
  );
  const [historyEntries, setHistoryEntries] = useState<HistoryEntry[]>([]);
  const [selectedCacheKeys, setSelectedCacheKeys] = useState<string[]>([]);
  const [coverCache, setCoverCache] = useState<Record<string, string>>({});
  const [resolveBanner, setResolveBanner] = useState<{ title: string; detail: string } | null>(null);
  const resolveGenerationRef = useRef(Date.now() * 1_000);
  const resolveAbortRef = useRef(false);
  const activeResolveRequestRef = useRef<string | null>(null);
  const cancelledResolveRequestsRef = useRef<Set<string>>(new Set());
  const suppressNextTerminalAdvanceRef = useRef(false);
  const terminalAdvanceGuardTimerRef = useRef<number | null>(null);
  const searchShellRef = useRef<HTMLDivElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const themePickerRef = useRef<HTMLDivElement | null>(null);
  const toastTimerRef = useRef<number | null>(null);

  const [searchQuery, setSearchQuery] = useState("");
  const [artistQuery, setArtistQuery] = useState("");
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
  const [chartLoading, setChartLoading] = useState(false);
  const [suggestionOpen, setSuggestionOpen] = useState(false);
  const [suggestionIndex, setSuggestionIndex] = useState(-1);
  const [playingCatalogKey, setPlayingCatalogKey] = useState<string | null>(null);

  const [selectedCatalogTrack, setSelectedCatalogTrack] = useState<CatalogTrack | null>(null);
  const [lyrics, setLyrics] = useState<LyricDocument | null>(null);
  const lyricsGenerationRef = useRef(0);
  const activeLyricsRequestRef = useRef<string | null>(null);
  const lyricRefs = useRef<Array<HTMLParagraphElement | null>>([]);

  /** Logical playlist (local paths + online CatalogTrack metadata). Online never pre-resolved. */
  const [playlist, setPlaylist] = useState<PlaylistEntry[]>(restoredPlaylistSession.playlist);
  const [playlistIndex, setPlaylistIndex] = useState<number | null>(restoredPlaylistSession.currentIndex);
  const [playlistSessionReady, setPlaylistSessionReady] = useState(false);
  const [queuePanelOpen, setQueuePanelOpen] = useState(false);
  const [localQueueAvailability, setLocalQueueAvailability] = useState<{
    status: QueueAvailabilityStatus;
    unavailablePaths: Set<string>;
  }>(() => ({
    status: localQueuePaths(restoredPlaylistSession.playlist).length ? "checking" : "ready",
    unavailablePaths: new Set<string>(),
  }));
  const [relinkingQueueIndex, setRelinkingQueueIndex] = useState<number | null>(null);
  const shufflePlayedRef = useRef<Set<number>>(new Set());
  const shuffleRngRef = useRef({ state: (Date.now() ^ 0x9e3779b9) >>> 0 || 1 });
  const advancingRef = useRef(false);
  const playlistRef = useRef(playlist);
  const playlistIndexRef = useRef(playlistIndex);
  const snapshotRef = useRef(snapshot);
  const mediaActionHandlerRef = useRef<(action: TransportAction) => void>(() => undefined);
  const transportCapabilitiesRef = useRef({ signature: "", revision: 0 });
  const localQueueAvailabilityGenerationRef = useRef(0);
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

  const checkLocalQueueAvailability = async (
    entries: PlaylistEntry[] = playlistRef.current,
    announce = false,
  ): Promise<void> => {
    const generation = ++localQueueAvailabilityGenerationRef.current;
    const paths = localQueuePaths(entries);
    if (!paths.length) {
      setLocalQueueAvailability({ status: "ready", unavailablePaths: new Set() });
      if (announce) setMessage("队列中没有需要检查的本地歌曲。");
      return;
    }

    setLocalQueueAvailability((state) => ({ ...state, status: "checking" }));
    try {
      const checks = await invoke<LocalPathAvailability[]>("library_check_local_paths", { paths });
      if (generation !== localQueueAvailabilityGenerationRef.current) return;
      const unavailablePaths = unavailablePathsFromChecks(entries, checks);
      setLocalQueueAvailability({ status: "ready", unavailablePaths });
      if (announce) {
        setMessage(unavailablePaths.size
          ? `检查完成：仍有 ${unavailablePaths.size} 首本地歌曲暂不可用。`
          : "本地歌曲路径已全部恢复可用。");
      }
    } catch (error) {
      if (generation !== localQueueAvailabilityGenerationRef.current) return;
      setLocalQueueAvailability((state) => ({ ...state, status: "failed" }));
      console.warn("[GXPlayer] local queue availability check failed", error);
      if (announce) setMessage(`本地歌曲检查失败，队列已保持不变：${String(error)}`, true);
    }
  };

  const refreshLibrary = async (scanMissing = false): Promise<LibraryTrack[]> => {
    const [tracks, favoriteTracks, nextPlaylists] = await Promise.all([
      invoke<LibraryTrack[]>(scanMissing ? "library_scan_missing" : "library_tracks"),
      invoke<LibraryTrack[]>("library_favorites"),
      invoke<PlaylistSummary[]>("library_playlists"),
    ]);
    setLibrary(tracks);
    setFavorites(favoriteTracks);
    setPlaylists(nextPlaylists);
    return tracks;
  };

  const loadChart = async () => {
    if (chartLoading || chartTracks.length > 0) return;
    setChartLoading(true);
    try {
      setChartTracks(await invoke<CatalogTrack[]>("metadata_chart", { limit: 12 }));
    } catch (error) {
      setChartTracks([]);
      setMessage(`在线榜单暂时不可用：${String(error)}`, true);
    } finally {
      setChartLoading(false);
    }
  };

  const refreshSources = async () => {
    const [nextSources, nextRuntime] = await Promise.all([
      invoke<ListedSource[]>("source_list"),
      invoke<RuntimeStatus>("source_status"),
    ]);
    setSources(nextSources);
    setRuntime(nextRuntime);
  };

  const refreshCache = async () => {
    const [status, favoriteTracks, entries, previewStatus] = await Promise.all([
      invoke<CacheStatus>("cache_status"),
      invoke<CatalogTrack[]>("cache_online_favorites"),
      invoke<CacheEntryView[]>("cache_list_entries"),
      invoke<PreviewCacheStatus>("preview_cache_status"),
    ]);
    setCacheStatus(status);
    if (!cacheLimitDirtyRef.current) {
      setCacheLimitGiB((status.limitBytes / 1024 / 1024 / 1024).toFixed(2).replace(/\.00$/, ""));
    }
    setOnlineFavorites(favoriteTracks);
    setCacheEntries(entries);
    setPreviewCacheStatus(previewStatus);
  };

  const refreshHistory = async () => {
    const entries = await invoke<HistoryEntry[]>("library_history", { limit: 500 });
    setHistoryEntries(entries);
  };

  const refreshOutputDevices = async () => {
    setOutputDeviceBusy(true);
    try {
      const status = await invoke<OutputDeviceStatus>("player_refresh_output_devices");
      setOutputDeviceStatus(status);
      setOutputDevices(status.devices);
    } finally {
      setOutputDeviceBusy(false);
    }
  };

  const selectOutputDevice = async (name: string | null) => {
    if (outputDeviceBusy) return;
    setOutputDeviceBusy(true);
    try {
      const status = await invoke<OutputDeviceStatus>("player_set_output_device", { name });
      setOutputDeviceStatus(status);
      setOutputDevices(status.devices);
      setAppPreferences((preferences) => preferences ? { ...preferences, outputDevice: status.selectedDevice } : preferences);
      setOutputDeviceFallback(null);
    } catch (error) {
      setMessage(String(error), true);
      await refreshOutputDevices().catch(() => undefined);
    } finally {
      setOutputDeviceBusy(false);
    }
  };

  const setCloseBehavior = async (behavior: CloseBehavior) => {
    try {
      setAppPreferences(await invoke<AppPreferences>("app_preferences_set_close_behavior", { behavior }));
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  const beginDiagnosticLogOperation = () => {
    diagnosticLogGenerationRef.current += 1;
    return diagnosticLogGenerationRef.current;
  };

  const isCurrentDiagnosticLogOperation = (generation: number) => (
    diagnosticLogGenerationRef.current === generation
  );

  const refreshDiagnosticLog = async (generation: number) => {
    const [status, entries] = await Promise.all([
      invoke<DiagnosticLogStatus>("diagnostic_log_status"),
      invoke<DiagnosticLogEntry[]>("diagnostic_log_recent", { limit: 100 }),
    ]);
    if (!isCurrentDiagnosticLogOperation(generation)) return false;
    setDiagnosticLogStatus(status);
    setDiagnosticLogEntries([...entries].reverse());
    return true;
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

  const supersedeActiveResolve = () => {
    const requestId = activeResolveRequestRef.current;
    if (!requestId) return;
    cancelledResolveRequestsRef.current.add(requestId);
    resolveAbortRef.current = true;
    resolveGenerationRef.current += 1;
    activeResolveRequestRef.current = null;
    void invoke("player_cancel_resolve", { requestId }).catch(() => undefined);
  };

  useEffect(() => {
    let disposed = false;
    // Window size is set once in Rust (setup) before first show — do not resize here
    // or the app will open at tauri.conf size then jump larger after React mounts.
    void invoke("ui_ready").catch((error) => setMessage(String(error), true));
    void refreshSources().catch((error) => setMessage(String(error), true));
    void refreshCache().catch((error) => setMessage(String(error), true));
    void refreshHistory().catch(() => undefined);
    void invoke<AppPreferences>("app_preferences_get")
      .then(setAppPreferences)
      .catch((error) => setMessage(String(error), true));

    void refreshLibrary(true).catch((error) => {
      console.warn("[GXPlayer] initial library scan failed", error);
    });
    void checkLocalQueueAvailability(restoredPlaylistSession.playlist);

    void (async () => {
      const session = restoredPlaylistSession;
      if (disposed) return;

      setPlaylist(session.playlist);
      setPlaylistIndex(session.currentIndex);
      try {
        await invoke("player_set_play_mode", { mode: session.playMode });
        if (!disposed) setSnapshot((state) => ({ ...state, playMode: session.playMode }));
      } catch (error) {
        console.warn("[GXPlayer] play mode restore failed", error);
      } finally {
        if (!disposed) setPlaylistSessionReady(true);
      }
    })();

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

    return () => {
      disposed = true;
      localQueueAvailabilityGenerationRef.current += 1;
    };
  }, []);

  useEffect(() => {
    if (view === "history") void refreshHistory().catch(() => undefined);
    if (view === "sources") void refreshSources().catch(() => undefined);
    if (view === "settings") {
      void refreshOutputDevices().catch((error) => setMessage(String(error), true));
      const generation = beginDiagnosticLogOperation();
      setDiagnosticLogBusy("refresh");
      void refreshDiagnosticLog(generation)
        .catch((error) => {
          if (isCurrentDiagnosticLogOperation(generation)) setMessage(String(error), true);
        })
        .finally(() => {
          if (isCurrentDiagnosticLogOperation(generation)) {
            setDiagnosticLogBusy((busy) => busy === "refresh" ? null : busy);
          }
        });
    } else {
      diagnosticLogGenerationRef.current += 1;
      setDiagnosticLogBusy(null);
    }
    if (view === "library") {
      void invoke<LibraryTrack[]>("library_scan_missing")
        .then(setLibrary)
        .catch(() => undefined);
    }
  }, [view]);

  useEffect(() => {
    let disposed = false;
    const unlisten = listen<string>("gx-source-capabilities-updated", () => {
      if (!disposed) void refreshSources().catch(() => undefined);
    });
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisten = listen<string>("gx-source-health-updated", () => {
      if (!disposed) void refreshSources().catch(() => undefined);
    });
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    const closeUnlisten = listen("gx-close-to-tray-notice-requested", () => {
      if (!disposed) setCloseNoticeOpen(true);
    });
    const fallbackUnlisten = listen<OutputDeviceFallbackEvent>("gx-output-device-fallback", (event) => {
      if (disposed) return;
      setOutputDeviceFallback(event.payload);
      setOutputDeviceStatus((status) => status ? { ...status, selectedDevice: null } : status);
      setAppPreferences((preferences) => preferences ? { ...preferences, outputDevice: null } : preferences);
    });
    return () => {
      disposed = true;
      void closeUnlisten.then((stop) => stop());
      void fallbackUnlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    if (!closeNoticeOpen) return;
    const frame = window.requestAnimationFrame(() => closeNoticeConfirmRef.current?.focus());
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || closeNoticeBusy) return;
      event.preventDefault();
      setCloseNoticeOpen(false);
      void invoke("app_close_notice_cancel").catch((error) => setMessage(String(error), true));
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [closeNoticeBusy, closeNoticeOpen]);

  useEffect(() => {
    if (!playlistSessionReady) return;
    savePlaylistSession({
      playlist,
      currentIndex: playlistIndex,
      playMode: snapshot.playMode,
    });
  }, [playlist, playlistIndex, playlistSessionReady, snapshot.playMode]);

  useEffect(() => {
    if (!playlistSessionReady) return;
    const persistNow = () => {
      savePlaylistSession({
        playlist: playlistRef.current,
        currentIndex: playlistIndexRef.current,
        playMode: snapshotRef.current.playMode,
      });
    };
    window.addEventListener("beforeunload", persistNow);
    window.addEventListener("pagehide", persistNow);
    return () => {
      window.removeEventListener("beforeunload", persistNow);
      window.removeEventListener("pagehide", persistNow);
    };
  }, [playlistSessionReady]);

  useEffect(() => {
    if (view !== "settings" && view !== "library") return;
    void refreshCache().catch((error) => pushMessage(String(error), true));
    let disposed = false;
    const unlisten = listen<number>("gx-cache-changed", () => {
      if (!disposed) void refreshCache().catch(() => undefined);
    });
    const previewUnlisten = listen<PreviewCacheStatus>("gx-preview-cache-changed", (event) => {
      if (!disposed) setPreviewCacheStatus(event.payload);
    });
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
      void previewUnlisten.then((stop) => stop());
    };
  }, [view]);

  useEffect(() => {
    if (view === "settings") void refreshProxyStatus();
  }, [refreshProxyStatus, view]);

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

  useEffect(() => {
    saveThemePreference(theme);
  }, [theme]);

  useEffect(() => {
    if (!themePickerOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const root = themePickerRef.current;
      if (!root) return;
      if (event.target instanceof Node && !root.contains(event.target)) {
        setThemePickerOpen(false);
      }
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        setThemePickerOpen(false);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [themePickerOpen]);

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

  const currentPlaylistEntry = playlistIndex === null ? null : playlist[playlistIndex] ?? null;
  const currentQueueItem = useMemo(
    () => (snapshot.queueIndex === null ? null : snapshot.queue[snapshot.queueIndex] ?? null),
    [snapshot.queue, snapshot.queueIndex],
  );
  const currentQueueKey = currentQueueItem ? `${snapshot.queueIndex}:${currentQueueItem.location}` : "";
  const currentLocalPath = currentQueueItem?.location
    ?? (currentPlaylistEntry?.kind === "local" ? currentPlaylistEntry.path : null);
  const currentLibraryTrack = useMemo(
    () => library.find((track) => track.path === currentLocalPath) ?? null,
    [currentLocalPath, library],
  );
  const queuedCatalogTrack = currentPlaylistEntry?.kind === "online" ? currentPlaylistEntry.track : null;
  const displayedCatalogTrack = selectedCatalogTrack ?? queuedCatalogTrack;
  const currentTitle = displayedCatalogTrack?.title
    ?? currentLibraryTrack?.title
    ?? (currentPlaylistEntry ? entryTitle(currentPlaylistEntry) : currentQueueItem?.title)
    ?? "尚未播放";
  const currentArtist = displayedCatalogTrack?.artist
    ?? currentLibraryTrack?.artist
    ?? (currentPlaylistEntry ? entryArtist(currentPlaylistEntry) : null)
    ?? "选择一首歌，让房间亮起来";
  const localCover = currentLibraryTrack?.path ? coverCache[currentLibraryTrack.path] ?? null : null;
  const currentArtworkUrl = displayedCatalogTrack?.artworkUrl ?? localCover;
  const currentArtwork = useArtworkUrl(currentArtworkUrl);
  const queuedDurationSeconds = currentPlaylistEntry?.kind === "local"
    ? currentPlaylistEntry.durationSeconds
    : currentPlaylistEntry?.kind === "online" && currentPlaylistEntry.track.durationMs !== null
      ? currentPlaylistEntry.track.durationMs / 1000
      : null;
  const currentDurationSeconds = snapshot.durationSeconds ?? queuedDurationSeconds;

  useEffect(() => {
    const path = currentLibraryTrack?.path;
    if (!path || coverCache[path] || displayedCatalogTrack?.artworkUrl) return;
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
  }, [currentLibraryTrack?.path, coverCache, displayedCatalogTrack?.artworkUrl]);
  useEffect(() => {
    const path = currentLibraryTrack?.path;
    if (!path) return;
    setCoverCache((prev) => prev[path] ? putLruValue(prev, path, prev[path], COVER_CACHE_LIMIT) : prev);
  }, [currentLibraryTrack?.path]);
  // Loading only while a session is opening — failed must not look like "still playing".
  const isPlaying = snapshot.status === "playing" || snapshot.status === "loading";
  const animatePlayback = snapshot.status === "playing";
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
  const { shownVolume, isAdjustingVolume, previewVolume, commitVolume } = useLiveVolume(
    snapshot.volume,
    (volume) => invoke("player_set_volume", { volume }),
    (volume) => invoke<AppPreferences>("player_commit_volume", { volume }).then(setAppPreferences),
    (error) => setMessage(String(error), true),
  );
  const measuredSourceSpec = formatSourceSpec(snapshot);
  const suspiciousQuality = isSuspiciousQuality(currentQuality, snapshot);
  const selectedOnlineFavorite = selectedCatalogTrack
    ? onlineFavorites.some((track) => track.providerId === selectedCatalogTrack.providerId && track.providerTrackId === selectedCatalogTrack.providerTrackId)
    : false;
  const groupedHistoryEntries = useMemo(() => groupConsecutiveHistory(historyEntries), [historyEntries]);
  const orderedSources = useMemo(
    () => [...sources].sort((left, right) => left.userPriority - right.userPriority),
    [sources],
  );
  const activeSource = orderedSources.find((source) => source.id === runtime?.activeSourceId)
    ?? orderedSources.find((source) => source.preferred)
    ?? null;
  const sourceStatus = (() => {
    switch (runtime?.state) {
      case "ready":
        return {
          title: "音源已就绪",
          copy: activeSource?.metadata.name ? `当前运行音源：${activeSource.metadata.name}` : "在线歌曲可解析为整首播放。",
        };
      case "initializing":
        return { title: "音源正在初始化", copy: activeSource?.metadata.name ? `正在启动：${activeSource.metadata.name}` : "请稍候，音源沙箱正在启动。" };
      case "failed":
        return { title: "音源启动失败", copy: runtime.error ?? "请检查音源脚本后重试。" };
      default:
        return { title: "还没有可用音源", copy: "导入 LX 音源脚本后，在线歌曲才能解析为整首播放。" };
    }
  })();

  useEffect(() => {
    if (!narrowLayout) setSidebarDrawerOpen(false);
  }, [narrowLayout]);

  useEffect(() => {
    if (!sidebarDrawerOpen) return;
    const frame = window.requestAnimationFrame(() => {
      sidebarRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    });
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setSidebarDrawerOpen(false);
      window.requestAnimationFrame(() => menuButtonRef.current?.focus());
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [sidebarDrawerOpen]);

  useEffect(() => {
    if (queuePanelOpen && sidebarDrawerOpen) setSidebarDrawerOpen(false);
  }, [queuePanelOpen, sidebarDrawerOpen]);

  const navigateTo = (next: ViewId) => {
    setSidebarDrawerOpen(false);
    if (next === view) return;
    setViewHistory((history) => [...history, view].slice(-32));
    setView(next);
  };

  const navigateBack = () => {
    setSidebarDrawerOpen(false);
    setViewHistory((history) => {
      const previous = history[history.length - 1];
      if (!previous) return history;
      setView(previous);
      return history.slice(0, -1);
    });
  };

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
    () => [...new Set(suggestions.flatMap((track) => splitArtistNames(track.artist)))].slice(0, 4),
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
    const requestId = activeLyricsRequestRef.current;
    activeLyricsRequestRef.current = null;
    if (requestId) {
      void invoke("metadata_cancel_request", { lane: "lyrics", requestId }).catch(() => undefined);
    }
    setLyrics(null);
  };

  const loadLyricsFor = async (title: string, artist: string, durationMs: number | null, baseMessage: string) => {
    const generation = ++lyricsGenerationRef.current;
    const previousRequestId = activeLyricsRequestRef.current;
    activeLyricsRequestRef.current = null;
    if (previousRequestId) {
      void invoke("metadata_cancel_request", {
        lane: "lyrics",
        requestId: previousRequestId,
      }).catch(() => undefined);
    }
    const requestId = nextLyricsRequestId();
    activeLyricsRequestRef.current = requestId;
    setLyrics(null);
    try {
      const lyricDocument = await invoke<LyricDocument | null>("metadata_lyrics", {
        title,
        artist,
        durationMs,
        requestId,
      });
      if (
        generation === lyricsGenerationRef.current
        && activeLyricsRequestRef.current === requestId
      ) setLyrics(lyricDocument);
    } catch (lyricError) {
      if (
        generation === lyricsGenerationRef.current
        && activeLyricsRequestRef.current === requestId
        && !isMetadataCancellation(lyricError)
      ) {
        setMessage(`${baseMessage} 歌曲已播放，但歌词加载失败：${String(lyricError)}`);
      }
    } finally {
      if (activeLyricsRequestRef.current === requestId) {
        activeLyricsRequestRef.current = null;
      }
    }
  };

  useEffect(() => () => {
    lyricsGenerationRef.current += 1;
    const requestId = activeLyricsRequestRef.current;
    activeLyricsRequestRef.current = null;
    if (requestId) {
      void invoke("metadata_cancel_request", { lane: "lyrics", requestId }).catch(() => undefined);
    }
  }, []);

  /**
   * Resolve and play a single online CatalogTrack into the engine.
   * Constraint 2: only called when the playhead actually reaches this track — never batch.
   * Supports explicit cancellation. The backend owns bounded per-stage timeouts so a
   * fixed client deadline cannot cut off later sources in the fallback chain.
   */
  const resolveAndPlayOnline = async (
    wanted: CatalogTrack,
    quality: QualityPreference,
    opts?: { allowPreviewFallback?: boolean; candidates?: CatalogTrack[] },
  ): Promise<PlaybackStartResult> => {
    const key = catalogKey(wanted);
    const generation = ++resolveGenerationRef.current;
    let failureKind: OnlinePlaybackResult["failureKind"] = null;
    const requestId = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${generation}-${Math.random().toString(16).slice(2)}`;
    resolveAbortRef.current = false;
    activeResolveRequestRef.current = requestId;
    suppressNextTerminalAdvanceRef.current = false;
    setPlayingCatalogKey(key);
    setResolveBanner({ title: `正在解析《${wanted.title}》`, detail: "可取消 · 仅解析当前这一首" });
    console.info("[GXPlayer] online resolve request", { key, requestId, title: wanted.title, quality });

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
      const online = await invoke<OnlinePlaybackResult>("player_play_online_track", {
        track: wanted,
        quality: quality === "auto" ? null : quality,
        sourceId: null,
        requestId,
        intentGeneration: generation,
      });
      const interrupted = interruptedOutcome();
      if (interrupted) return interrupted;
      if (online.outcome === "cancelled" || online.outcome === "stale") {
        return { outcome: online.outcome };
      }
      if (online.outcome === "failed") {
        failureKind = online.failureKind ?? "unknown";
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
        return { outcome: "failed", error: onlineError, failureKind: failureKind ?? "unknown" };
      }
      try {
        const preview = await invoke<{ track: CatalogTrack; replacedProviderId: string | null }>("metadata_play_preview", {
          wanted,
          candidates: opts.candidates ?? [wanted],
          requestId,
          intentGeneration: generation,
        });
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
        return { outcome: "failed", error: previewError, failureKind: failureKind ?? "unknown" };
      }
    } finally {
      cancelledResolveRequestsRef.current.delete(requestId);
      if (activeResolveRequestRef.current === requestId) activeResolveRequestRef.current = null;
      if (generation === resolveGenerationRef.current) {
        setResolveBanner(null);
        setPlayingCatalogKey(null);
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

  const playHistoryEntry = async (entry: HistoryEntry) => {
    supersedeActiveResolve();
    if (entry.kind === "local" && entry.path) {
      const track = { id: -1, path: entry.path, title: entry.title, artist: entry.artist, album: "", durationSeconds: null, favorite: false, addedAtMs: 0 };
      await playLocalInList([track], track);
      return;
    }
    if (!entry.providerId || !entry.providerTrackId) return;
    try {
      const quality = await invoke<string | null>("player_play_history_cache", {
        request: {
          providerId: entry.providerId,
          providerTrackId: entry.providerTrackId,
          quality: entry.quality,
          title: entry.title,
          artist: entry.artist,
        },
      });
      if (quality) {
        const cached: Extract<PlaylistEntry, { kind: "cached" }> = {
          kind: "cached",
          providerId: entry.providerId,
          providerTrackId: entry.providerTrackId,
          quality,
          title: entry.title,
          artist: entry.artist,
        };
        setPlaylist([cached]);
        setPlaylistIndex(0);
        setSelectedCatalogTrack(cacheEntryToCatalog({
          providerId: cached.providerId,
          providerTrackId: cached.providerTrackId,
          quality,
          title: cached.title,
          artist: cached.artist,
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
        setCurrentQuality(quality);
        clearLyrics();
        setMessage(`历史记录已从本地缓存播放 · ${quality}`);
        return;
      }
      if (entry.kind === "cached") {
        setMessage(`《${entry.title}》的缓存已不存在，未发起联网请求。`, true);
        return;
      }
      await playCatalog({
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
    } catch (error) {
      setMessage(`播放历史记录失败：${String(error)}`, true);
    }
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
      if (localQueueAvailability.unavailablePaths.has(entry.path)) {
        const error = new Error(`《${entry.title}》的本地文件暂不可用`);
        setMessage("本地文件暂不可用；接回磁盘后请在播放队列中重试，或重新定位文件。", true);
        return { outcome: "failed", error };
      }
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
    return resolveAndPlayOnline(entry.track, entry.quality, {
      allowPreviewFallback: opts?.allowPreviewFallback,
      candidates: entries
        .filter((item): item is Extract<PlaylistEntry, { kind: "online" }> => item.kind === "online")
        .map((item) => item.track),
    });
  };

  /**
   * Advance the playhead. Only a track-scoped "no playable URL" may be skipped,
   * and a single chain can skip at most three tracks.
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
    let failureSkipCount = 0;
    try {
      const mode = snapshotRef.current.playMode ?? "sequential";
      let cursor = current;
      let pausedForExplicitAdvance = false;
      for (let attempt = 0; attempt < Math.max(entries.length, 1); attempt += 1) {
        const isFailureSkip = Boolean(opts?.fromFailure) || attempt > 0;
        if (isFailureSkip && failureSkipCount >= MAX_CONSECUTIVE_FAILURE_SKIPS) {
          setMessage(`已连续跳过 ${MAX_CONSECUTIVE_FAILURE_SKIPS} 首无可用地址的歌曲，已停止自动尝试。`, true);
          return { outcome: "failed", failureKind: "track_unavailable" };
        }
        const next = isFailureSkip
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
        if (!pausedForExplicitAdvance && (intent === "next" || intent === "previous")) {
          try {
            // Stop feeding the old online stream before resolving the explicitly requested item.
            // Natural end never enters this branch.
            await invoke("player_pause");
            pausedForExplicitAdvance = true;
          } catch (error) {
            setMessage(`切歌前暂停当前播放失败：${String(error)}`, true);
            return { outcome: "failed", error };
          }
        }
        tried.add(next);
        if (isFailureSkip) failureSkipCount += 1;
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
    return playPlaylistEntry(entries, index, opts);
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

      const failureNote = result.failures.length ? `，另有 ${result.failures.length} 个文件导入失败` : "";
      setMessage(`已导入 ${result.imported.length} 首到曲库，当前播放和队列未改变${failureNote}`);
    } catch (error) {
      setMessage(String(error), true);
    }
  };

  /** Click a local track: load the entire current view as the queue, start at the clicked item. */
  const playLocalInList = async (tracks: LibraryTrack[], track: LibraryTrack) => {
    supersedeActiveResolve();
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

  const relinkLocalQueueEntry = async (index: number) => {
    if (relinkingQueueIndex !== null) return;
    const entry = playlistRef.current[index];
    if (!entry || entry.kind !== "local") return;
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "音频", extensions: ["mp3", "flac", "wav", "m4a", "aac", "ogg"] }],
    });
    if (!selected || Array.isArray(selected)) return;

    setRelinkingQueueIndex(index);
    try {
      const relinked = await invoke<LibraryTrack>("library_relink_track", {
        oldPath: entry.path,
        newPath: selected,
      });
      const replacement = localEntryFromLibrary(relinked);
      const nextEntries = relinkLocalQueuePath(playlistRef.current, entry.path, replacement);
      setPlaylist(nextEntries);
      await checkLocalQueueAvailability(nextEntries);
      void refreshLibrary(true).catch((error) => {
        console.warn("[GXPlayer] library refresh after relink failed", error);
      });
      setMessage(`已重新定位《${entry.title}》`);
    } catch (error) {
      setMessage(`重新定位失败：${String(error)}`, true);
    } finally {
      setRelinkingQueueIndex(null);
    }
  };

  /** Click a catalog track: queue the whole list as online placeholders; resolve only the clicked one. */
  const playCatalogInList = async (tracks: CatalogTrack[], wanted: CatalogTrack) => {
    supersedeActiveResolve();
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
    supersedeActiveResolve();
    const entries = playlistRef.current;
    const target = entries[index];
    if (!target) return;
    if (target.kind === "local" && localQueueAvailability.unavailablePaths.has(target.path)) {
      setMessage("这首歌的本地文件暂不可用；请先重试检查或重新定位。", true);
      return;
    }
    shufflePlayedRef.current.add(index);
    setPlaylistIndex(index);
    if (playlistIsLocalOnly(entries) && target.kind === "local") {
      try {
        if (engineMatchesLocalQueue(entries, snapshotRef.current.queue)) {
          await invoke("player_jump", { index });
        } else {
          await invoke("player_load_local", {
            paths: entries.map((entry) => (entry as Extract<PlaylistEntry, { kind: "local" }>).path),
            startIndex: index,
          });
        }
        setSelectedCatalogTrack(null);
        setCurrentQuality(null);
        clearLyrics();
        void recordHistory({ kind: "local", title: target.title, artist: target.artist, path: target.path });
      } catch (error) {
        setMessage(formatFailureMessage(error, target.title), true);
      }
      return;
    }
    await playPlaylistEntry(entries, index);
  };

  const playCacheInList = async (entries: CacheEntryView[], wanted: CacheEntryView) => {
    supersedeActiveResolve();
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
    const removedEntry = entries[index];
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
    if (removedEntry?.kind === "local") {
      void checkLocalQueueAvailability(entries);
    }
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
    localQueueAvailabilityGenerationRef.current += 1;
    setLocalQueueAvailability({ status: "ready", unavailablePaths: new Set() });
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
    const text = JSON.stringify({ version: 2, library: libraryBackup, sources: sourceBackup }, null, 2);
    resetBackupRestorePreview();
    setBackupText(text);
    const path = await save({
      defaultPath: "gxplayer-backup.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    await invoke("backup_write_file", { path, content: text });
    setMessage(`备份已写入 ${path}`);
  };

  const exportUnmatchedTextPlaylist = async (content: string) => {
    const path = await save({
      defaultPath: "gxplayer-text-import-unmatched.txt",
      filters: [{ name: "文本列表", extensions: ["txt"] }],
    });
    if (!path || Array.isArray(path)) return;
    await invoke("backup_write_file", { path, content });
    setMessage(`未匹配条目已导出到 ${path}`);
  };

  const refreshDiagnosticLogNow = async () => {
    if (diagnosticLogBusy) return;
    const generation = beginDiagnosticLogOperation();
    setDiagnosticLogBusy("refresh");
    try {
      const applied = await refreshDiagnosticLog(generation);
      if (applied) setMessage("诊断日志已刷新。");
    } catch (error) {
      if (isCurrentDiagnosticLogOperation(generation)) setMessage(String(error), true);
    } finally {
      if (isCurrentDiagnosticLogOperation(generation)) setDiagnosticLogBusy(null);
    }
  };

  const setDiagnosticLogEnabled = async (enabled: boolean) => {
    if (diagnosticLogBusy) return;
    const generation = beginDiagnosticLogOperation();
    setDiagnosticLogBusy("toggle");
    try {
      const status = await invoke<DiagnosticLogStatus>("diagnostic_log_set_enabled", { enabled });
      if (!isCurrentDiagnosticLogOperation(generation)) return;
      setDiagnosticLogStatus(status);
      await refreshDiagnosticLog(generation);
      if (isCurrentDiagnosticLogOperation(generation)) {
        setMessage(status.enabled ? "诊断日志已开启。" : "诊断日志已关闭；已有记录仍保留在本地。");
      }
    } catch (error) {
      if (isCurrentDiagnosticLogOperation(generation)) setMessage(String(error), true);
    } finally {
      if (isCurrentDiagnosticLogOperation(generation)) setDiagnosticLogBusy(null);
    }
  };

  const exportDiagnosticLog = async () => {
    if (diagnosticLogBusy) return;
    const generation = beginDiagnosticLogOperation();
    setDiagnosticLogBusy("export");
    try {
      const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
      const path = await save({
        defaultPath: `gxplayer-diagnostic-${timestamp}.jsonl`,
        filters: [{ name: "JSON Lines", extensions: ["jsonl"] }],
      });
      if (!path || Array.isArray(path)) return;
      const result = await invoke<DiagnosticLogExportResult>("diagnostic_log_export", { path });
      if (isCurrentDiagnosticLogOperation(generation)) {
        setMessage(`已导出 ${result.entryCount} 条诊断日志到 ${result.path}`);
      }
    } catch (error) {
      if (isCurrentDiagnosticLogOperation(generation)) setMessage(String(error), true);
    } finally {
      if (isCurrentDiagnosticLogOperation(generation)) setDiagnosticLogBusy(null);
    }
  };

  const clearDiagnosticLog = async () => {
    if (diagnosticLogBusy || !window.confirm("确定清空全部诊断日志吗？此操作无法撤销。")) return;
    const generation = beginDiagnosticLogOperation();
    setDiagnosticLogBusy("clear");
    try {
      await invoke("diagnostic_log_clear");
      if (!isCurrentDiagnosticLogOperation(generation)) return;
      setDiagnosticLogEntries([]);
      await refreshDiagnosticLog(generation);
      if (isCurrentDiagnosticLogOperation(generation)) setMessage("诊断日志已清空。");
    } catch (error) {
      if (isCurrentDiagnosticLogOperation(generation)) setMessage(String(error), true);
    } finally {
      if (isCurrentDiagnosticLogOperation(generation)) setDiagnosticLogBusy(null);
    }
  };

  const importBackupFile = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    const content = await invoke<string>("backup_read_file", { path });
    resetBackupRestorePreview();
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
      // Engine failures are not proven to be track-scoped. Stop here instead of sweeping the
      // online queue on a network/output/system fault; the engine error toast remains visible.
      setPlaylistIndex(current);
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
        intentGeneration: generation,
        preserveTransport: true,
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
      setMessage(online.cacheHit ? `已切换到本地缓存 ${online.quality ?? "自动"}，播放位置已保留。` : `已切换到 ${online.quality ?? "自动"}，播放位置已保留。`);
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

  const importSourceFile = async () => {
    if (sourceImportBusy) return;
    setSourceImportBusy("file");
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "LX 音源脚本", extensions: ["js"] }],
      });
      if (!selected || Array.isArray(selected)) return;
      await invoke("source_import_file", { path: selected });
      await refreshSources();
      setMessage("已从本地文件导入音源脚本。");
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceImportBusy(null);
    }
  };

  const importSourceUrl = async () => {
    const url = sourceUrl.trim();
    if (!url || sourceImportBusy) return;
    setSourceImportBusy("url");
    try {
      await invoke("source_import_url", { url });
      setSourceUrl("");
      await refreshSources();
      setMessage("已从 URL 导入音源脚本。");
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceImportBusy(null);
    }
  };

  const saveSourceOrder = async (nextOrderedSources: ListedSource[]) => {
    if (sourceOrderBusy || sourceActionBusy) return;
    const previousSources = sources;
    const optimisticSources = nextOrderedSources.map((source, userPriority) => ({
      ...source,
      userPriority,
    }));
    setSources(optimisticSources);
    setSourceOrderBusy(true);
    try {
      await invoke("source_set_order", { sourceIds: optimisticSources.map((source) => source.id) });
      await refreshSources();
      setMessage("音源偏好顺序已保存；实际选源仍会优先选择健康状态更好的音源。");
    } catch (error) {
      setSources(previousSources);
      setMessage(`调整音源顺序失败，已恢复原顺序：${String(error)}`, true);
    } finally {
      setSourceOrderBusy(false);
    }
  };

  const moveSource = (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (target < 0 || target >= orderedSources.length) return;
    const next = [...orderedSources];
    [next[index], next[target]] = [next[target]!, next[index]!];
    void saveSourceOrder(next);
  };

  const dropSource = (targetId: string) => {
    const sourceId = draggedSource;
    setDraggedSource(null);
    if (!sourceId || sourceId === targetId) return;
    const next = [...orderedSources];
    const from = next.findIndex((source) => source.id === sourceId);
    const to = next.findIndex((source) => source.id === targetId);
    if (from < 0 || to < 0) return;
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved!);
    void saveSourceOrder(next);
  };

  const setSourceEnabled = async (source: ListedSource, enabled: boolean) => {
    if (sourceActionBusy || sourceOrderBusy) return;
    setSourceActionBusy({ id: source.id, kind: "toggle" });
    try {
      await invoke("source_set_enabled", { id: source.id, enabled });
      await refreshSources();
      setMessage(enabled ? `已启用音源“${source.metadata.name || source.id}”。` : `已禁用音源“${source.metadata.name || source.id}”。`);
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceActionBusy(null);
    }
  };

  const reimportSource = async (source: ListedSource) => {
    if (sourceActionBusy || sourceOrderBusy) return;
    setSourceActionBusy({ id: source.id, kind: "reimport" });
    try {
      await invoke("source_reimport", { id: source.id });
      await refreshSources();
      setMessage(`已重新导入音源“${source.metadata.name || source.id}”。`);
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceActionBusy(null);
    }
  };

  const removeSource = async (source: ListedSource) => {
    if (sourceActionBusy || sourceOrderBusy) return;
    if (!window.confirm(`确定删除音源“${source.metadata.name || source.id}”吗？`)) return;
    setSourceActionBusy({ id: source.id, kind: "remove" });
    try {
      await invoke("source_remove", { id: source.id });
      await refreshSources();
      setMessage(`已删除音源“${source.metadata.name || source.id}”。`);
    } catch (error) {
      setMessage(String(error), true);
    } finally {
      setSourceActionBusy(null);
    }
  };

  const openSourceConfig = async (source: ListedSource) => {
    if (sourceConfigBusy || sourceActionBusy || sourceOrderBusy) return;
    setSourceConfigBusy(true);
    try {
      const config = await invoke<Record<string, unknown>>("source_get_config", { id: source.id });
      setConfigSource(source);
      setSourceConfigDraft({
        json: JSON.stringify(config, null, 2),
        enabled: source.enabled,
        updatesEnabled: source.updatesEnabled,
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
      const config: unknown = JSON.parse(sourceConfigDraft.json);
      if (!config || typeof config !== "object" || Array.isArray(config)) {
        throw new Error("音源配置必须是一个 JSON 对象");
      }
      await invoke("source_set_config", { id: configSource.id, config });
      if (sourceConfigDraft.enabled !== configSource.enabled) {
        await invoke("source_set_enabled", { id: configSource.id, enabled: sourceConfigDraft.enabled });
      }
      if (sourceConfigDraft.updatesEnabled !== configSource.updatesEnabled) {
        await invoke("source_set_updates_enabled", { id: configSource.id, enabled: sourceConfigDraft.updatesEnabled });
      }
      closeSourceConfig();
      await refreshSources();
      setMessage(sourceConfigDraft.enabled ? "音源设置已保存并应用。" : "音源设置已保存；该音源当前不参与音源调用。");
    } catch (error) {
      const detail = error instanceof SyntaxError ? `配置 JSON 格式有误：${error.message}` : String(error);
      setMessage(detail, true);
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

  const openArtistPage = (rawArtist: string) => {
    const artist = rawArtist.trim();
    if (!artist) return;
    setArtistQuery(artist);
    setSearchQuery(artist);
    setSuggestionOpen(false);
    setSuggestionIndex(-1);
    navigateTo("artist");
    void searchCatalog(artist);
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
    if (option.kind === "artist") {
      openArtistPage(option.query);
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
      setPlaylistItems([]);
      navigateTo("playlist");
    }
  };

  const openPlaylist = async (playlist: PlaylistSummary) => {
    const items = await run<LibraryPlaylistItem[]>("library_playlist_items", { playlistId: playlist.id });
    if (items) {
      setActivePlaylist(playlist);
      setPlaylistItems(items);
      navigateTo("playlist");
    }
  };

  const addToPlaylist = async (trackId: number, playlistId: number) => {
    await run("library_add_to_playlist", { trackId, playlistId });
    await refreshLibrary();
  };

  const addCachedToPlaylist = async (entry: CacheEntryView, playlistId: number) => {
    await run("library_add_cached_to_playlist", {
      playlistId,
      providerId: entry.providerId,
      providerTrackId: entry.providerTrackId,
      quality: entry.quality,
      title: entry.title,
      artist: entry.artist,
      album: entry.album,
    });
    await refreshLibrary();
  };

  const playLibraryPlaylistItem = async (items: LibraryPlaylistItem[], index: number) => {
    const target = items[index];
    if (!target) return;
    if (target.kind === "local" && (
      target.track.missing
      || library.some((track) => track.path === target.track.path && track.missing)
    )) {
      setMessage("这首歌的本地文件暂不可用，请重新导入或重新定位后再试。", true);
      return;
    }
    if (target.kind === "cached" && !availableCacheKeys.has(
      cachedIdentityKey(target.providerId, target.providerTrackId, target.quality),
    )) {
      setMessage("这首歌的缓存已被清理；歌单记录仍保留，未发起联网请求。", true);
      return;
    }
    supersedeActiveResolve();
    const result = await replacePlaylist(items.map(libraryPlaylistItemToQueueEntry), index);
    if (result.outcome === "started") navigateTo("now-playing");
  };

  const exportBackup = async () => {
    const [libraryBackup, sourceBackup] = await Promise.all([
      invoke("library_export_backup"),
      invoke("source_export_backup"),
    ]);
    resetBackupRestorePreview();
    setBackupText(JSON.stringify({ version: 2, library: libraryBackup, sources: sourceBackup }, null, 2));
  };

  const {
    preview: backupRestorePreview,
    busy: backupRestoreBusy,
    inspect: inspectBackupRestore,
    restore: restoreBackup,
    resetPreview: resetBackupRestorePreview,
  } = useBackupRestore({
    backupText,
    onRestored: async () => {
      await Promise.all([refreshLibrary(), refreshSources()]);
    },
    onMessage: setMessage,
  });

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
          <button
            className="icon-button"
            onClick={() => void enqueueLocalTracks([track])}
            aria-label={track.missing ? `${track.title} 的文件缺失，无法添加到队列` : `将 ${track.title} 添加到队列`}
            title={track.missing ? "文件缺失，无法添加" : "添加到队列"}
            disabled={Boolean(track.missing)}
          >＋</button>
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
            <ArtistLinks artist={track.artist} onSelect={openArtistPage} className="catalog-artist-links" />
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
            className="icon-button cache-enqueue"
            onClick={() => enqueueCacheEntries([entry])}
            aria-label="添加到队列"
            title="添加到队列"
          >
            ＋
          </button>
          <button
            type="button"
            className={`icon-button cache-pin ${entry.pinned ? "active" : ""}`}
            onClick={() => void toggleCachePinned(entry)}
            aria-label={entry.pinned ? "取消钉住" : "收藏钉住"}
            title={entry.pinned ? "取消钉住" : "收藏并钉住"}
          >
            {entry.pinned ? "♥" : "♡"}
          </button>
          <select className="cache-playlist-select" aria-label={`将 ${entry.title} 添加到歌单`} defaultValue="" onChange={(event) => {
            const playlistId = Number(event.target.value);
            if (playlistId) void addCachedToPlaylist(entry, playlistId);
            event.target.value = "";
          }}>
            <option value="">＋ 歌单</option>
            {playlists.map((item) => <option value={item.id} key={item.id}>{item.name}</option>)}
          </select>
          <button
            type="button"
            className="icon-button cache-remove"
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

  const renderLibraryPlaylistItems = (items: LibraryPlaylistItem[], playlistId: number) => (
    <div className="track-list" role="list">
      {items.map((item, index) => {
        const isLocal = item.kind === "local";
        const title = isLocal ? item.track.title : item.title;
        const artist = (isLocal ? item.track.artist : item.artist) || "未知歌手";
        const album = isLocal ? item.track.album : item.album;
        const cacheAvailable = isLocal || availableCacheKeys.has(
          cachedIdentityKey(item.providerId, item.providerTrackId, item.quality),
        );
        const unavailable = isLocal
          ? Boolean(item.track.missing || library.some((track) => track.path === item.track.path && track.missing))
          : !cacheAvailable;
        const key = isLocal
          ? `local:${item.track.id}`
          : `cached:${cachedIdentityKey(item.providerId, item.providerTrackId, item.quality)}`;
        return (
          <div className="track-row playlist-item-row" role="listitem" key={key}>
            <button
              type="button"
              className="track-main"
              disabled={unavailable}
              onClick={() => void playLibraryPlaylistItem(items, index)}
            >
              <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
              <span>
                <strong>{title}{unavailable ? isLocal ? " · 文件缺失" : " · 缓存不可用" : ""}</strong>
                <small>
                  {artist}{album ? ` · ${album}` : ""}
                  {isLocal ? " · 本地" : ` · 缓存 ${item.quality}`}
                  {unavailable ? " · 记录已保留，不会自动联网" : ""}
                </small>
              </span>
            </button>
            {isLocal
              ? <time>{formatTime(item.track.durationSeconds)}</time>
              : <span className="cache-quality-badge">{item.quality}</span>}
            <button
              type="button"
              className="icon-button"
              disabled={unavailable}
              aria-label={`将 ${title} 添加到队列`}
              title={unavailable ? "当前不可用" : "添加到队列"}
              onClick={() => {
                const entry = libraryPlaylistItemToQueueEntry(item);
                const wasEmpty = playlistRef.current.length === 0;
                setPlaylist((current) => [...current, entry]);
                if (wasEmpty) setPlaylistIndex(0);
                setMessage(`已将《${title}》添加到队列`);
              }}
            >＋</button>
            <button
              type="button"
              className="icon-button"
              aria-label={`从歌单移除 ${title}`}
              onClick={async () => {
                if (isLocal) {
                  await run("library_remove_from_playlist", { playlistId, trackId: item.track.id });
                } else {
                  await run("library_remove_cached_from_playlist", {
                    playlistId,
                    providerId: item.providerId,
                    providerTrackId: item.providerTrackId,
                    quality: item.quality,
                  });
                }
                if (activePlaylist) await openPlaylist(activePlaylist);
                await refreshLibrary();
              }}
            >×</button>
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
            className={`mini-stage ${snapshot.audioMode === "music" ? "bypassed" : "enabled"} ${animatePlayback ? "is-playing" : ""}`}
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
        <section className="section-block panel-enter delay-2">
          <div className="section-heading">
            <div><p className="eyebrow">DISCOVER</p><h2>正在流行</h2></div>
            {chartTracks.length > 0 && <button onClick={() => { seedResults(chartTracks, "中国区热门"); setSearchQuery("中国区热门"); navigateTo("search"); }}>查看全部 →</button>}
          </div>
          {chartTracks.length > 0
            ? renderCatalogRows(chartTracks.slice(0, 6))
            : <EmptyState
                title={chartLoading ? "正在加载在线榜单" : "在线榜单尚未加载"}
                copy="为了保持启动安静，在线内容会在你明确需要时再联网获取。"
                action={chartLoading ? undefined : "加载在线榜单"}
                onAction={() => void loadChart()}
              />}
        </section>
      </div>
    );

    if (view === "search") return (
      <div className="page">
        <PageHeading eyebrow="SEARCH" title={resultsQuery ? `“${resultsQuery}” 的结果` : "搜索音乐"} copy={runtime?.state === "ready" ? `${sourceStatus.copy} 点击歌曲将优先解析整首播放，失败时会明确提示并回退官方 30 秒预览。` : `${sourceStatus.title}：${sourceStatus.copy} 当前仍可尝试官方 30 秒预览。`} />
        {resultsState === "loading" && !searchResults.length ? (
          <LoadingState />
        ) : resultsState === "error" ? (
          <ErrorState title="搜索没有完成" copy={resultsError ?? "请检查网络或音源后重试。"} onRetry={retryResults} />
        ) : searchResults.length ? (
          <>
            {resultsState === "loading" && <div className="search-progress"><i className="search-spinner" />已有结果，仍在搜索其他平台…</div>}
            {renderCatalogRows(searchResults)}
          </>
        ) : resultsState === "empty" ? (
          <EmptyState title="没有找到相关音乐" copy="换一个歌名、歌手或专辑关键词试试。" />
        ) : (
          <EmptyState title="从顶栏开始搜索" copy="输入歌名、歌手或专辑，联想结果会按类型分组。" />
        )}
      </div>
    );

    if (view === "artist") return (
      <div className="page">
        <PageHeading
          eyebrow="ARTIST SEARCH"
          title={artistQuery ? `歌手：${artistQuery}` : "歌手搜索"}
          copy="以下是按歌手名搜索的结果，可能包含同名、翻唱或相关条目，不是该歌手的权威作品全集。"
        />
        {resultsState === "loading" && !searchResults.length ? (
          <LoadingState />
        ) : resultsState === "error" ? (
          <ErrorState title="歌手搜索没有完成" copy={resultsError ?? "请检查网络后重试。"} onRetry={retryResults} />
        ) : searchResults.length ? (
          <>
            {resultsState === "loading" && <div className="search-progress"><i className="search-spinner" />已有结果，仍在搜索其他平台…</div>}
            {renderCatalogRows(searchResults)}
          </>
        ) : resultsState === "empty" ? (
          <EmptyState title="没有找到相关音乐" copy="换一个歌手名试试。" />
        ) : (
          <EmptyState title="还没有开始搜索" copy="从搜索联想中点击歌手名即可查看结果。" />
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
            action={<div className="page-heading-actions"><button type="button" onClick={() => setTextPlaylistDialogOpen(true)}>导入文本列表</button><button className="primary" onClick={chooseFiles}>导入音乐</button></div>}
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
            copy={`${historyEntries.length} 条原始播放记录，连续同曲合并显示为 ${groupedHistoryEntries.length} 行（读取最近 500 条）。`}
            action={<button type="button" className="danger" onClick={async () => { if (!window.confirm("确定清空全部播放历史吗？")) return; try { await invoke("library_clear_history"); await refreshHistory(); } catch (error) { setMessage(String(error), true); } }}>清空历史</button>}
          />
          {historyEntries.length === 0 ? (
            <EmptyState title="还没有播放记录" copy="听歌后会出现在这里，方便找回昨晚那首。" />
          ) : (
            <div className="track-list" role="list">
              {groupedHistoryEntries.map(({ entry, count }) => (
                <div className="track-row history-row" role="listitem" key={entry.id}>
                  <button
                    type="button"
                    className="track-main"
                    onClick={() => void playHistoryEntry(entry)}
                  >
                    <span className="track-index">{entry.kind.slice(0, 2)}</span>
                    <span>
                      <strong>{entry.title}</strong>
                      <small>{entry.artist || "未知歌手"} · {new Date(entry.playedAtMs).toLocaleString()}</small>
                    </span>
                  </button>
                  {count > 1 && <span className="history-count" aria-label={`连续播放 ${count} 次`}>×{count}</span>}
                </div>
              ))}
            </div>
          )}
        </div>
      );
    }

    if (view === "playlist") return (
      <div className="page"><PageHeading eyebrow="PLAYLIST" title={activePlaylist?.name ?? "歌单"} copy={`${playlistItems.length} 首音乐 · 支持本地与已缓存歌曲`} action={activePlaylist ? <button className="danger" onClick={async () => { if (!window.confirm(`确定删除歌单“${activePlaylist.name}”吗？`)) return; try { await invoke("library_delete_playlist", { playlistId: activePlaylist.id }); navigateTo("discovery"); setActivePlaylist(null); setPlaylistItems([]); await refreshLibrary(); } catch (error) { setMessage(String(error), true); } }}>删除歌单</button> : undefined} />{playlistItems.length && activePlaylist ? renderLibraryPlaylistItems(playlistItems, activePlaylist.id) : <EmptyState title="这个歌单还没有歌" copy="回到曲库，把本地音乐或已缓存歌曲加进来。" action="去曲库" onAction={() => navigateTo("library")} />}</div>
    );

    if (view === "sources") return (
      <div className="page"><PageHeading eyebrow="MUSIC SOURCES" title="管理音源" copy="拖动卡片设置偏好顺序；实际请求会先按健康状态分档，再按你的顺序选择。" action={<button disabled={Boolean(sourceImportBusy)} onClick={() => void importSourceFile()}>{sourceImportBusy === "file" ? "正在导入…" : "从本地文件导入"}</button>} />
        <section className="source-import-band" aria-labelledby="source-import-title">
          <div className="source-import-copy">
            <p className="eyebrow">IMPORT</p>
            <h2 id="source-import-title">从 URL 导入</h2>
            <p>应用不内置任何音源目录或链接。仅下载你主动提供的脚本，并在隔离沙箱中运行。</p>
          </div>
          <form className="inline-form source-url-form" onSubmit={(event) => { event.preventDefault(); void importSourceUrl(); }}>
            <input type="url" aria-label="音源脚本 URL" placeholder="https://example.com/source.js" autoComplete="off" spellCheck={false} value={sourceUrl} disabled={Boolean(sourceImportBusy)} onChange={(event) => setSourceUrl(event.target.value)} />
            <button type="submit" className="primary" disabled={!sourceUrl.trim() || Boolean(sourceImportBusy)}>{sourceImportBusy === "url" ? "正在导入…" : "导入 URL"}</button>
          </form>
        </section>
        <section className="source-status-card"><span className={`runtime-dot ${runtime?.state ?? "no_source"}`} /><div><strong>{sourceStatus.title}</strong><p>{sourceStatus.copy}</p></div><code>GEN {runtime?.generation ?? 0}</code></section>
        <div className="source-list-heading">
          <div><h2>音源优先序</h2><p>绿灯优先于黄灯、红灯；同一健康档位内按这里的顺序降级。</p></div>
          <span>{orderedSources.filter((source) => source.enabled).length} / {orderedSources.length} 已启用</span>
        </div>
        <p className="source-health-note">健康度只记录真实解析调用结果，不会主动探测。双击卡片可编辑完整设置。</p>
        {orderedSources.length ? (
          <div className="source-list">
            {orderedSources.map((source, index) => (
              <SourceCard
                key={source.id}
                source={source}
                index={index}
                total={orderedSources.length}
                dragging={draggedSource === source.id}
                busy={sourceOrderBusy || Boolean(sourceActionBusy) || sourceConfigBusy}
                reimporting={sourceActionBusy?.id === source.id && sourceActionBusy.kind === "reimport"}
                onDragStart={() => setDraggedSource(source.id)}
                onDragEnd={() => setDraggedSource(null)}
                onDrop={() => dropSource(source.id)}
                onMove={(direction) => moveSource(index, direction)}
                onEdit={() => void openSourceConfig(source)}
                onToggle={() => void setSourceEnabled(source, !source.enabled)}
                onReimport={() => void reimportSource(source)}
                onRemove={() => void removeSource(source)}
              />
            ))}
          </div>
        ) : <div className="source-empty-state">还没有导入音源。可从本地文件或你提供的 URL 导入脚本。</div>}
      </div>
    );

    if (view === "settings") return (
      <div className="page"><PageHeading eyebrow="SETTINGS" title="设置与备份" copy="输出设备、窗口和本地数据都在这里管理。" />
        <div className="settings-grid">
          <section className="settings-card"><h3>输出设备</h3><p>进入设置时会重新枚举；设备断开后自动回退到系统默认设备。</p><select value={outputDeviceStatus?.selectedDevice ?? appPreferences?.outputDevice ?? ""} disabled={outputDeviceBusy} onChange={(event) => void selectOutputDevice(event.target.value || null)}><option value="">系统默认设备{outputDeviceStatus?.defaultDevice ? ` · ${outputDeviceStatus.defaultDevice}` : ""}</option>{outputDevices.map((device) => <option key={device} value={device}>{device}</option>)}</select><button type="button" disabled={outputDeviceBusy} onClick={() => void refreshOutputDevices().catch((error) => setMessage(String(error), true))}>{outputDeviceBusy ? "正在刷新…" : "重新枚举设备"}</button></section>
          <section className="settings-card"><h3>默认音质</h3><p>自动会按当前平台能力从高到低尝试，并在解析失败时逐档回退。</p><select value={qualityPreference} onChange={(event) => updateQualityPreference(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></section>
          <section className="settings-card"><h3>听感模式</h3><p>仅本次运行有效；重启后恢复原声直通，避免意外带入空间处理。</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></section>
          <section className="settings-card proxy-settings">
            <h3>网络代理</h3>
            <p>复用你本机操作系统配置的第三方代理服务，非本应用提供。音源连接优先直连，失败时才按需回退到代理。</p>
            <label className="settings-toggle">
              <span><strong>允许按需使用系统代理</strong><small>关闭后所有连接纯直连；开启或自动时，音源会记住最近成功的直连/代理路由。</small></span>
              <input
                type="checkbox"
                checked={proxyStatus ? proxyStatus.mode !== "off" : false}
                disabled={!proxyStatus || proxyBusy}
                onChange={(event) => void setProxyMode(event.target.checked ? "on" : "off")}
              />
            </label>
            <div className="proxy-status-line">
              <span>{proxyStatus?.mode === "auto" ? "自动允许" : proxyStatus?.mode === "on" ? "手动允许" : proxyStatus?.mode === "off" ? "仅直连" : "正在读取"}</span>
              <span>{proxyStatus ? (proxyStatus.detected && proxyStatus.mode !== "off" ? "已检测到，可在直连失败时使用" : proxyStatus.detected ? "已检测到，但当前不允许使用" : "未检测到系统代理，当前直连") : "正在检测系统代理"}</span>
              <button type="button" disabled={!proxyStatus || proxyBusy || proxyStatus.mode === "auto"} onClick={() => void setProxyMode("auto")}>恢复自动检测</button>
            </div>
          </section>
          <section className="settings-card">
            <h3>窗口</h3>
            <p>位置与尺寸会自动记忆；关闭行为可随时修改。</p>
            <label><span>关闭按钮（X）</span><select value={appPreferences?.closeBehavior ?? "hide_to_tray"} disabled={!appPreferences} onChange={(event) => void setCloseBehavior(event.target.value as CloseBehavior)}><option value="hide_to_tray">隐藏到系统托盘</option><option value="exit">退出应用</option></select></label>
            <small>{appPreferences?.closeBehavior === "exit" ? "点击 X 会结束播放并退出。" : "点击 X 后继续后台播放；托盘右键菜单提供显式退出。"}</small>
            <div className="cache-actions">
              <button type="button" className={alwaysOnTop ? "primary" : ""} onClick={() => void toggleAlwaysOnTop()}>{alwaysOnTop ? "取消置顶" : "窗口置顶"}</button>
              <button type="button" className={miniMode ? "primary" : ""} onClick={() => void toggleMiniMode()}>{miniMode ? "退出迷你" : "迷你模式"}</button>
            </div>
          </section>
          <section className="settings-card cache-settings"><h3>在线播放缓存</h3><p>只保存自然播放时已经收到的字节，不会预抓或批量下载。试听缓存独立限制为 256 MiB，并按最近使用自动淘汰。</p><dl><div><dt>完整歌曲</dt><dd>{cacheStatus ? `${formatBytes(cacheStatus.totalBytes)} · ${cacheStatus.entryCount} 项` : "读取中…"}</dd></div><div><dt>试听缓存</dt><dd>{previewCacheStatus ? `${formatBytes(previewCacheStatus.totalBytes)} · ${previewCacheStatus.entryCount} 项 / ${formatBytes(previewCacheStatus.limitBytes)}` : "读取中…"}</dd></div><div><dt>收藏钉住</dt><dd>{cacheStatus?.pinnedCount ?? 0} 项</dd></div><div><dt>目录</dt><dd title={cacheStatus?.directory}>{cacheStatus?.directory ?? "读取中…"}</dd></div></dl><label><span>完整歌曲上限（GiB）</span><div className="inline-form"><input type="number" min="0.125" step="0.5" value={cacheLimitGiB} onChange={(event) => { cacheLimitDirtyRef.current = true; setCacheLimitGiB(event.target.value); }} /><button onClick={() => void saveCacheLimit()}>保存</button></div></label><div className="cache-actions"><button onClick={() => void chooseCacheDirectory()}>选择目录</button><button onClick={async () => { const status = await invoke<CacheStatus>("cache_reset_directory"); setCacheStatus(status); setMessage("已恢复默认缓存目录；旧目录内容未迁移。"); }}>恢复默认</button><button onClick={async () => { const status = await invoke<PreviewCacheStatus>("preview_cache_clear"); setPreviewCacheStatus(status); setMessage("试听缓存已清理。"); }}>清理试听</button><button onClick={async () => { if (!window.confirm("确定清理所有未收藏缓存吗？")) return; const status = await invoke<CacheStatus>("cache_clear", { includePinned: false }); setCacheStatus(status); }}>清未收藏</button><button className="danger" onClick={async () => { if (!window.confirm("确定清空全部缓存（包括收藏钉住项）吗？")) return; const status = await invoke<CacheStatus>("cache_clear", { includePinned: true }); setCacheStatus(status); }}>清空全部</button></div></section>
          <section className="settings-card diagnostic-log-settings">
            <div className="diagnostic-log-heading">
              <div>
                <h3>诊断日志</h3>
                <p>默认开启，只记录异常和关键路由事件，不记录正常流水。敏感 URL、音源 key 等会脱敏，本地日志采用双文件约 2 MiB 轮转。</p>
              </div>
              <div className="diagnostic-log-actions">
                <button type="button" disabled={Boolean(diagnosticLogBusy)} onClick={() => void refreshDiagnosticLogNow()}>{diagnosticLogBusy === "refresh" ? "正在刷新…" : "刷新"}</button>
                <button type="button" disabled={Boolean(diagnosticLogBusy) || diagnosticLogEntries.length === 0} onClick={() => void exportDiagnosticLog()}>{diagnosticLogBusy === "export" ? "正在导出…" : "导出 JSONL"}</button>
                <button type="button" className="danger" disabled={Boolean(diagnosticLogBusy) || diagnosticLogEntries.length === 0} onClick={() => void clearDiagnosticLog()}>{diagnosticLogBusy === "clear" ? "正在清空…" : "清空"}</button>
              </div>
            </div>
            <label className="settings-toggle diagnostic-log-toggle">
              <span>
                <strong>{diagnosticLogStatus ? (diagnosticLogStatus.enabled ? "日志已开启" : "日志已关闭") : "正在读取日志状态"}</strong>
                <small>关闭后停止写入新记录；已有日志仍可查看、导出或清空。</small>
              </span>
              <input type="checkbox" checked={diagnosticLogStatus?.enabled ?? false} disabled={!diagnosticLogStatus || Boolean(diagnosticLogBusy)} onChange={(event) => void setDiagnosticLogEnabled(event.target.checked)} />
            </label>
            <div className="diagnostic-log-meta">
              <span className={diagnosticLogStatus?.enabled ? "enabled" : "disabled"}>{diagnosticLogStatus?.enabled ? "记录中" : "未记录"}</span>
              <span>最近显示 {diagnosticLogEntries.length} / 100 条</span>
              <span>最新记录在前 · 本地双文件约 2 MiB 轮转</span>
            </div>
            {diagnosticLogEntries.length ? (
              <ol className="diagnostic-log-list">
                {diagnosticLogEntries.map((entry, index) => {
                  const timestamp = new Date(entry.timestampMs);
                  const display = diagnosticEntryDisplay(entry);
                  return (
                    <li key={`${entry.timestampMs}:${entry.category}:${index}`}>
                      <time dateTime={timestamp.toISOString()}>{timestamp.toLocaleString()}</time>
                      <span className="diagnostic-log-category" title={display.category}>{display.category}</span>
                      <span className="diagnostic-log-source" title={display.source}>{display.source}</span>
                      <p title={display.summary}>{display.summary}</p>
                    </li>
                  );
                })}
              </ol>
            ) : (
              <div className="diagnostic-log-empty">{diagnosticLogBusy === "refresh" ? "正在读取最近日志…" : diagnosticLogStatus?.enabled ? "目前没有异常或关键事件。" : "日志已关闭，暂无可显示记录。"}</div>
            )}
          </section>
        </div>
        <section className="backup-card">
          <div className="section-heading">
            <div>
              <h3>配置备份</h3>
              <p>包含本地曲库、歌单、音源脚本及音源密钥；可存为文件或从文件读入。备份内容请勿公开。</p>
            </div>
            <div className="cache-actions">
              <button type="button" disabled={Boolean(backupRestoreBusy)} onClick={() => void exportBackup()}>生成到文本框</button>
              <button type="button" disabled={Boolean(backupRestoreBusy)} onClick={() => void exportBackupFile()}>存为文件…</button>
              <button type="button" disabled={Boolean(backupRestoreBusy)} onClick={() => void importBackupFile()}>从文件读入…</button>
              {backupRestorePreview ? (
                <button type="button" className="primary" disabled={Boolean(backupRestoreBusy)} onClick={() => void restoreBackup()}>
                  {backupRestoreBusy === "restore" ? "正在恢复…" : "确认覆盖并恢复"}
                </button>
              ) : (
                <button type="button" className="primary" disabled={!backupText.trim() || Boolean(backupRestoreBusy)} onClick={() => void inspectBackupRestore()}>
                  {backupRestoreBusy === "preview" ? "正在校验…" : "检查备份"}
                </button>
              )}
            </div>
          </div>
          {backupRestorePreview && (
            <div className="backup-restore-preview" role="status" aria-live="polite">
              <strong>恢复预览</strong>
              <span>将覆盖 {backupRestorePreview.trackCount} 首曲目 / {backupRestorePreview.playlistCount} 个歌单 / {backupRestorePreview.sourceCount} 个音源</span>
              <small>下一步仍会要求确认；恢复失败时会自动回滚到当前数据。</small>
            </div>
          )}
          <textarea
            aria-label="GXPlayer 备份 JSON"
            placeholder="生成的备份会显示在这里，也可以粘贴已有备份。"
            value={backupText}
            disabled={Boolean(backupRestoreBusy)}
            onChange={(event) => {
              resetBackupRestorePreview();
              setBackupText(event.target.value);
            }}
          />
        </section>
      </div>
    );

    return (
      <div className="page now-playing-page">
        <div className="now-grid">
          <section className={`record-column ${animatePlayback ? "is-playing" : ""}`}>
            <div className={`record-stage ${animatePlayback ? "live" : ""}`}>
              <div className="record-glow" aria-hidden="true" />
              <div className={`record ${animatePlayback ? "spinning" : ""}`}>
                <Cover artwork={currentArtwork} title={currentTitle} className="record-cover" eager />
                <span className="record-hole" />
              </div>
              <div className={`eq-bars ${animatePlayback ? "active" : ""}`} aria-hidden="true">
                <i /><i /><i /><i /><i />
              </div>
            </div>
            <p className="eyebrow">NOW PLAYING</p>
            <h1 className={animatePlayback ? "title-live" : ""}>{currentTitle}</h1>
            {displayedCatalogTrack?.artist ? (
              <ArtistLinks artist={displayedCatalogTrack.artist} onSelect={openArtistPage} className="artist-line artist-line-links" />
            ) : <p className="artist-line">{currentArtist}</p>}
            {measuredSourceSpec && (
              <p className={`source-spec ${suspiciousQuality ? "suspicious" : ""}`}>
                {currentQuality && currentQueueItem?.online ? <><span>{currentQuality}（自报）</span><b>·</b></> : null}
                <span>实测 {measuredSourceSpec}</span>
                {suspiciousQuality && <em title="自报高解析音质与解码规格不一致">⚠ 疑似虚标</em>}
              </p>
            )}
          </section>
          <section className="stage-panel">
            <div className={`sound-stage ${snapshot.audioMode === "music" ? "bypassed" : "enabled"} ${animatePlayback ? "is-playing" : ""}`} aria-label="声场模式盘">
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
    <div className={`app-shell ${!narrowLayout && sidebarCollapsed ? "sidebar-collapsed" : ""} ${narrowLayout ? "narrow-layout" : ""} ${miniMode ? "mini-mode" : ""} ${isMaximized ? "is-maximized" : ""} ${windowActive ? "" : "app-idle"}`} data-theme={theme} style={{ "--accent": accent } as CSSProperties}>
      <div className="ambient-light" aria-hidden="true" />
      <div className="ambient-light ambient-light-secondary" aria-hidden="true" />
      <div className="shell-noise" aria-hidden="true" />
      <header className="top-bar" data-tauri-drag-region>
        <div className="brand-cluster">
          <button
            ref={menuButtonRef}
            className="menu-button"
            onClick={() => {
              if (narrowLayout) {
                setQueuePanelOpen(false);
                setSidebarDrawerOpen((open) => !open);
              } else {
                setSidebarCollapsed((value) => !value);
              }
            }}
            aria-controls="app-sidebar"
            aria-expanded={narrowLayout ? sidebarDrawerOpen : undefined}
            aria-pressed={narrowLayout ? undefined : !sidebarCollapsed}
            aria-label={narrowLayout
              ? sidebarDrawerOpen ? "关闭导航抽屉" : "打开导航抽屉"
              : sidebarCollapsed ? "展开侧栏" : "收起侧栏"}
          >☰</button>
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
          <div className="theme-picker" ref={themePickerRef}>
            <button
              type="button"
              className={`theme-trigger ${themePickerOpen ? "active" : ""}`}
              aria-label="切换皮肤"
              aria-haspopup="menu"
              aria-expanded={themePickerOpen}
              title="切换皮肤"
              onClick={() => setThemePickerOpen((open) => !open)}
            >
              <span aria-hidden="true">◐</span>
              <span className="theme-trigger-label">换肤</span>
            </button>
            {themePickerOpen && (
              <div className="theme-menu" role="menu" aria-label="选择皮肤">
                {THEME_OPTIONS.map((option) => (
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={theme === option.id}
                    className={`theme-option theme-option-${option.id} ${theme === option.id ? "selected" : ""}`}
                    key={option.id}
                    onClick={() => {
                      setTheme(option.id);
                      setThemePickerOpen(false);
                    }}
                  >
                    <span className="theme-swatch" aria-hidden="true" />
                    <span className="theme-option-copy">
                      <strong>{option.label}</strong>
                      <small>{option.description}</small>
                    </span>
                    <span className="theme-option-check" aria-hidden="true">{theme === option.id ? "✓" : ""}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
        <div className="window-controls"><button onClick={() => void getCurrentWindow().minimize()} aria-label="最小化">─</button><button className="maximize-control" onClick={() => void getCurrentWindow().toggleMaximize()} aria-label="最大化">□</button><button className="close" onClick={() => void getCurrentWindow().close()} aria-label={appPreferences?.closeBehavior === "exit" ? "退出应用" : "隐藏到系统托盘"} title={appPreferences?.closeBehavior === "exit" ? "退出应用" : "隐藏到系统托盘"}>×</button></div>
      </header>

      {narrowLayout && sidebarDrawerOpen && (
        <button
          type="button"
          className="sidebar-drawer-backdrop"
          tabIndex={-1}
          aria-label="关闭导航抽屉"
          onClick={() => {
            setSidebarDrawerOpen(false);
            window.requestAnimationFrame(() => menuButtonRef.current?.focus());
          }}
        />
      )}
      {(!narrowLayout || sidebarDrawerOpen) && (
        <aside
          ref={sidebarRef}
          id="app-sidebar"
          className={`sidebar ${narrowLayout ? "sidebar-drawer" : ""}`}
          aria-label="主导航"
        >
          <nav>{NAV_ITEMS.map((item) => <button className={view === item.id ? "active" : ""} onClick={() => navigateTo(item.id)} key={item.id} title={item.label}><span>{item.icon}</span><strong>{item.label}</strong></button>)}</nav>
          <div className="sidebar-playlists"><p><span>创建的歌单</span><small>{playlists.length}</small></p>{playlists.slice(0, 8).map((playlist) => <button key={playlist.id} className={activePlaylist?.id === playlist.id && view === "playlist" ? "active" : ""} onClick={() => void openPlaylist(playlist)} title={playlist.name}><span>♬</span><strong>{playlist.name}</strong></button>)}</div>
          <div className="engine-health"><i className={snapshot.status === "failed" ? "bad" : ""} /><span><strong>Rust Engine</strong><small>{snapshot.status === "failed" ? "需要处理" : `${snapshot.underrunCallbacks} underrun`}</small></span></div>
        </aside>
      )}

      <main className="content">{renderView()}</main>

      {configSource && sourceConfigDraft && (
        <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeSourceConfig(); }}>
          <section className="config-modal" role="dialog" aria-modal="true" aria-label={`${configSource.metadata.name} 音源配置`}>
            <div className="section-heading">
              <div><p className="eyebrow">SOURCE SETTINGS</p><h3>{configSource.metadata.name || "音源设置"}</h3><p>配置结构由音源脚本定义；应用不会猜测或改写其中字段。</p></div>
              <button onClick={closeSourceConfig} aria-label="关闭配置">×</button>
            </div>
            <div className="config-toggles">
              <label><span><strong>启用音源</strong><small>禁用后不参与音源调用和自动降级。</small></span><input type="checkbox" checked={sourceConfigDraft.enabled} onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, enabled: event.target.checked })} /></label>
              <label><span><strong>更新提醒</strong><small>保留现有音源更新提示设置。</small></span><input type="checkbox" checked={sourceConfigDraft.updatesEnabled} onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, updatesEnabled: event.target.checked })} /></label>
            </div>
            <div className="config-json-section">
              <div><strong>完整配置 JSON</strong><p>可能包含密钥等敏感内容，默认隐藏；显示后请勿截图或分享。</p></div>
              <button type="button" aria-expanded={sourceConfigRevealed} onClick={() => setSourceConfigRevealed((revealed) => !revealed)}>{sourceConfigRevealed ? "隐藏配置" : "显示配置"}</button>
            </div>
            {sourceConfigRevealed && (
              <label className="config-json-editor">
                <span>按 JSON 对象原样保存</span>
                <textarea className="config-editor" value={sourceConfigDraft.json} autoComplete="off" spellCheck={false} onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, json: event.target.value })} />
              </label>
            )}
            <div className="modal-actions"><button onClick={closeSourceConfig}>取消</button><button className="primary" disabled={sourceConfigBusy} onClick={() => void saveSourceConfig()}>{sourceConfigBusy ? "正在保存…" : "保存并应用"}</button></div>
          </section>
        </div>
      )}

      {closeNoticeOpen && (
        <div className="modal-backdrop" role="presentation">
          <section className="config-modal close-to-tray-modal" role="dialog" aria-modal="true" aria-labelledby="close-to-tray-title" aria-describedby="close-to-tray-copy">
            <div className="section-heading">
              <div><p className="eyebrow">BACKGROUND PLAYBACK</p><h3 id="close-to-tray-title">关闭后继续播放</h3></div>
            </div>
            <p id="close-to-tray-copy">GXPlayer 关闭后会隐藏到系统托盘，音乐继续播放。左键托盘图标恢复，右键菜单可退出；也可在设置中修改。</p>
            <div className="modal-actions">
              <button type="button" disabled={closeNoticeBusy} onClick={() => {
                setCloseNoticeBusy(true);
                void invoke("app_close_notice_cancel")
                  .then(() => setCloseNoticeOpen(false))
                  .catch((error) => setMessage(String(error), true))
                  .finally(() => setCloseNoticeBusy(false));
              }}>暂不关闭</button>
              <button ref={closeNoticeConfirmRef} type="button" className="primary" disabled={closeNoticeBusy} onClick={() => {
                setCloseNoticeBusy(true);
                void invoke<AppPreferences>("app_close_notice_confirm")
                  .then((preferences) => {
                    setAppPreferences(preferences);
                    setCloseNoticeOpen(false);
                  })
                  .catch((error) => setMessage(String(error), true))
                  .finally(() => setCloseNoticeBusy(false));
              }}>{closeNoticeBusy ? "正在处理…" : "知道了，隐藏到托盘"}</button>
            </div>
          </section>
        </div>
      )}

      <TextPlaylistImportDialog
        open={textPlaylistDialogOpen}
        onClose={() => setTextPlaylistDialogOpen(false)}
        onEnqueue={enqueueCatalogTracks}
        onExportUnmatched={exportUnmatchedTextPlaylist}
        invoke={invoke}
      />

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

      {outputDeviceFallback && (
        <div className="device-fallback-banner" role="alert">
          <span>!</span>
          <p>{outputDeviceFallback.fallbackDevice
            ? `“${outputDeviceFallback.unavailableDevice}”已断开，已切换到系统默认设备“${outputDeviceFallback.fallbackDevice}”。`
            : `“${outputDeviceFallback.unavailableDevice}”已断开，且没有可用的系统默认输出设备。`}</p>
          <button type="button" onClick={() => setOutputDeviceFallback(null)} aria-label="关闭输出设备提示">×</button>
        </div>
      )}

      <footer className="player-bar">
        <div className="player-progress-rail">
          <input
            aria-label="播放进度"
            type="range"
            className="seek-slider"
            min={0}
            max={Math.max(currentDurationSeconds ?? 0, 0.01)}
            step={0.05}
            value={Math.min(shownPosition, Math.max(currentDurationSeconds ?? 0, 0.01))}
            disabled={!currentQueueItem || !snapshot.durationSeconds}
            style={
              {
                "--fill": `${currentDurationSeconds ? (Math.min(shownPosition, currentDurationSeconds) / currentDurationSeconds) * 100 : 0}%`,
              } as CSSProperties
            }
            onChange={(event) => setDragPosition(Number(event.target.value))}
            onPointerUp={(event) => void commitSeek(Number(event.currentTarget.value))}
            onKeyUp={(event) => {
              if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) void commitSeek(Number(event.currentTarget.value));
            }}
          />
        </div>
        <button className={`player-track ${animatePlayback ? "is-playing" : ""}`} onClick={() => navigateTo("now-playing")}>
          <span className={`player-cover-wrap ${animatePlayback ? "live" : ""}`}>
            <Cover artwork={currentArtwork} title={currentTitle} eager />
            {animatePlayback && <span className="player-eq" aria-hidden="true"><i /><i /><i /></span>}
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
          <div className="timeline player-time-row">
            <time>{formatTime(shownPosition)}</time>
            <span aria-hidden="true" />
            <time>{formatTime(currentDurationSeconds)}</time>
          </div>
        </div>
        <div className="player-tools">
          {selectedCatalogTrack && currentQueueItem?.online && <button className={`online-favorite ${selectedOnlineFavorite ? "active" : ""}`} onClick={() => void toggleOnlineFavorite(selectedCatalogTrack)} aria-label={selectedOnlineFavorite ? "取消在线收藏" : "收藏在线歌曲"} title={selectedOnlineFavorite ? "取消收藏" : "收藏并钉住缓存"}>{selectedOnlineFavorite ? "♥" : "♡"}</button>}
          <span
            className={`measured-quality ${suspiciousQuality ? "suspicious" : ""} ${measuredSourceSpec ? "" : "is-placeholder"}`}
            role={measuredSourceSpec ? "img" : undefined}
            tabIndex={measuredSourceSpec ? 0 : -1}
            aria-hidden={measuredSourceSpec ? undefined : true}
            aria-label={measuredSourceSpec ? `${currentQuality ? `${currentQuality}（音源自报） · ` : ""}实测 ${measuredSourceSpec}${suspiciousQuality ? " · 疑似虚标" : ""}` : undefined}
            title={measuredSourceSpec ? `${currentQuality ? `${currentQuality}（音源自报） · ` : ""}实测 ${measuredSourceSpec}${suspiciousQuality ? " · 疑似虚标" : ""}` : undefined}
          >
            {measuredSourceSpec && <span aria-hidden="true">{suspiciousQuality ? "!" : "i"}</span>}
          </span>
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
              onChange={(event) => previewVolume(Number(event.target.value))}
              onPointerUp={(event) => {
                const volume = Number(event.currentTarget.value);
                commitVolume(volume);
              }}
              onPointerCancel={(event) => {
                commitVolume(Number(event.currentTarget.value));
              }}
              onKeyUp={(event) => {
                if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) {
                  commitVolume(Number(event.currentTarget.value));
                }
              }}
              onBlur={(event) => {
                if (isAdjustingVolume) commitVolume(Number(event.currentTarget.value));
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
        availabilityStatus={localQueueAvailability.status}
        rows={displayPlaylist.map((entry, index) => ({
          key: entryKey(entry, index),
          title: entryTitle(entry),
          subtitle: `${entryArtist(entry)} · ${entrySourceLabel(entry)}${entry.kind === "online" && index !== displayIndex ? " · 待解析" : ""}${entry.kind === "local" && localQueueAvailability.unavailablePaths.has(entry.path) ? " · 暂不可用" : ""}`,
          active: index === displayIndex,
          unavailable: entry.kind === "local" && localQueueAvailability.unavailablePaths.has(entry.path),
          relinking: index === relinkingQueueIndex,
        }))}
        onClose={() => setQueuePanelOpen(false)}
        onClear={() => void clearPlaylist()}
        onJump={(index) => void jumpToPlaylistIndex(index)}
        onRelink={(index) => void relinkLocalQueueEntry(index)}
        onRetryAvailability={() => void checkLocalQueueAvailability(playlistRef.current, true)}
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

function SourceCard({
  source,
  index,
  total,
  dragging,
  busy,
  reimporting,
  onDragStart,
  onDragEnd,
  onDrop,
  onMove,
  onEdit,
  onToggle,
  onReimport,
  onRemove,
}: {
  source: ListedSource;
  index: number;
  total: number;
  dragging: boolean;
  busy: boolean;
  reimporting: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDrop: () => void;
  onMove: (direction: -1 | 1) => void;
  onEdit: () => void;
  onToggle: () => void;
  onReimport: () => void;
  onRemove: () => void;
}) {
  const displayName = source.metadata.name || "未命名音源";
  const effectivePriority = source.effectivePriority === null ? "不参与" : `#${source.effectivePriority + 1}`;
  return (
    <article
      className={`source-card ${source.preferred ? "preferred" : ""} ${source.enabled ? "" : "disabled"} ${dragging ? "dragging" : ""}`.trim()}
      draggable={!busy}
      onDragStart={(event) => {
        if (busy) { event.preventDefault(); return; }
        event.dataTransfer.effectAllowed = "move";
        onDragStart();
      }}
      onDragOver={(event) => { if (!busy) event.preventDefault(); }}
      onDrop={(event) => { event.preventDefault(); if (!busy) onDrop(); }}
      onDragEnd={onDragEnd}
      onDoubleClick={(event) => {
        const target = event.target;
        if (target instanceof HTMLElement && target.closest("button, input, label")) return;
        onEdit();
      }}
      aria-label={`${displayName}，用户顺序第 ${source.userPriority + 1}，实际优先级 ${effectivePriority}`}
    >
      <div className="source-order-column" title="拖动卡片调整偏好顺序">
        <span>{index + 1}</span>
        <small>偏好</small>
        <i aria-hidden="true">⋮⋮</i>
      </div>
      <div className="source-card-main">
        <div className="source-card-heading">
          {source.preferred && <span className="source-badge preferred">当前实际首选</span>}
          <span className={`source-badge ${source.enabled ? "enabled" : "disabled"}`}>{source.enabled ? "已启用" : "已禁用"}</span>
          {source.hasConfig && <span className="source-badge configured">有配置</span>}
          <SourceHealthIndicator health={source.health} />
        </div>
        <h3>{displayName}</h3>
        <p>{source.metadata.author || "未知作者"} · v{source.metadata.version || "?"}</p>
        <SourceCapabilityDetails capabilities={source.capabilities} />
        <div className="source-priority-summary">
          <span>用户顺序 <strong>#{source.userPriority + 1}</strong></span>
          <span>实际优先 <strong>{effectivePriority}</strong></span>
        </div>
      </div>
      <div className="source-actions" draggable={false} onDragStart={(event) => { event.preventDefault(); event.stopPropagation(); }} onDoubleClick={(event) => event.stopPropagation()}>
        <div className="source-order-buttons">
          <button type="button" disabled={busy || index === 0} onClick={() => onMove(-1)} aria-label={`上移 ${displayName}`}>↑</button>
          <button type="button" disabled={busy || index === total - 1} onClick={() => onMove(1)} aria-label={`下移 ${displayName}`}>↓</button>
        </div>
        <button type="button" disabled={busy} onClick={onToggle}>{source.enabled ? "禁用" : "启用"}</button>
        <button type="button" disabled={busy} onClick={onEdit}>编辑</button>
        <button type="button" disabled={busy} onClick={onReimport}>{reimporting ? "重新导入…" : "重新导入"}</button>
        <button type="button" className="danger" disabled={busy} onClick={onRemove}>删除</button>
      </div>
    </article>
  );
}

function SourceCapabilityDetails({ capabilities }: Pick<ListedSource, "capabilities">) {
  const platforms = capabilities.map((capability) => capability.platform);
  const qualities = [...new Set(capabilities.flatMap((capability) => capability.qualities))];
  return <dl className="source-capabilities">
    <div><dt>平台</dt><dd>{platforms.length ? platforms.join(" / ") : "未提供"}</dd></div>
    <div><dt>音质</dt><dd>{qualities.length ? qualities.join(" / ") : "未提供"}</dd></div>
  </dl>;
}

function SourceHealthIndicator({ health }: Pick<ListedSource, "health">) {
  const stateLabels: Record<typeof health.state, string> = {
    unknown: "暂无样本",
    healthy: "稳定",
    degraded: "偶有波动",
    unhealthy: "近期失败较多",
  };
  const detail = health.sampleCount === 0
    ? "尚未发生可统计的真实解析调用"
    : `最近 ${health.sampleCount} 次：成功 ${health.successRatePercent ?? 0}% · 平均 ${health.averageLatencyMs ?? 0} ms · 最近一次 ${health.lastSuccess ? "成功" : "失败"}`;
  return (
    <span className={`source-health source-health-${health.state}`} title={`${stateLabels[health.state]}：${detail}`} aria-label={`音源健康度：${stateLabels[health.state]}`}>
      <i aria-hidden="true" />
      <span>{stateLabels[health.state]}</span>
    </span>
  );
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
