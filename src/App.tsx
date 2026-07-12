import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "@fontsource-variable/noto-sans-sc";
import gxplayerIcon from "./assets/gxplayer-icon.png";
import "./App.css";
import {
  EMPTY_ENGINE,
  type CacheStatus,
  type CatalogTrack,
  type EngineSnapshot,
  type LibraryTrack,
  type ListedSource,
  type LyricDocument,
  type OnlinePlaybackResult,
  type PlayMode,
  type PlaylistSummary,
  type RuntimeStatus,
  type ViewId,
} from "./types";

type SearchState = "idle" | "loading" | "ready" | "empty" | "error";
type AudioMode = EngineSnapshot["audioMode"];
type QualityPreference = "auto" | "128k" | "320k" | "flac" | "flac24bit";
type SourceConfigDraft = {
  lsConfig: Record<string, unknown>;
  constName: string;
  keyValue: string;
  apiAddr: string;
  apiPass: string;
};

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
    };

const PLAY_MODE_ORDER: PlayMode[] = ["sequential", "repeat_all", "repeat_one", "shuffle"];
const PLAY_MODE_META: Record<PlayMode, { label: string; glyph: string }> = {
  sequential: { label: "顺序播放", glyph: "seq" },
  repeat_all: { label: "列表循环", glyph: "all" },
  repeat_one: { label: "单曲循环", glyph: "one" },
  shuffle: { label: "随机播放", glyph: "shuf" },
};

function catalogKey(track: CatalogTrack): string {
  return `${track.providerId}:${track.providerTrackId}`;
}

function entryKey(entry: PlaylistEntry, index: number): string {
  if (entry.kind === "local") return `local:${entry.path}:${index}`;
  return `online:${catalogKey(entry.track)}:${index}`;
}

function entryTitle(entry: PlaylistEntry): string {
  return entry.kind === "local" ? entry.title : entry.track.title;
}

function entryArtist(entry: PlaylistEntry): string {
  return entry.kind === "local" ? entry.artist || "未知歌手" : entry.track.artist || "未知歌手";
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

/** Frontend-side next index for online/mixed playlists (engine only holds the resolved current track). */
function pickPlaylistIndex(
  mode: PlayMode,
  current: number,
  length: number,
  intent: "ended" | "next" | "previous",
  shufflePlayed: Set<number>,
): number | null {
  if (length <= 0) return null;
  if (intent === "previous") {
    if (mode === "shuffle") {
      shufflePlayed.add(current);
      return pickShuffle(length, shufflePlayed, current);
    }
    if (mode === "repeat_all") return current === 0 ? length - 1 : current - 1;
    return current > 0 ? current - 1 : 0;
  }
  if (mode === "repeat_one" && intent === "ended") return current;
  if (mode === "shuffle") {
    shufflePlayed.add(current);
    return pickShuffle(length, shufflePlayed, current);
  }
  if (mode === "repeat_all") return (current + 1) % length;
  const next = current + 1;
  return next < length ? next : null;
}

function pickShuffle(length: number, played: Set<number>, preferNot: number): number {
  let available = Array.from({ length }, (_, i) => i).filter((i) => !played.has(i));
  if (available.length === 0) {
    played.clear();
    available = Array.from({ length }, (_, i) => i);
    if (length > 1) available = available.filter((i) => i !== preferNot);
  }
  if (available.length === 0) return 0;
  const choice = available[Math.floor(Math.random() * available.length)]!;
  played.add(choice);
  return choice;
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
  { id: "search", icon: "⌕", label: "搜索" },
  { id: "library", icon: "♫", label: "本地曲库" },
  { id: "favorites", icon: "♥", label: "收藏" },
  { id: "sources", icon: "◈", label: "音源管理" },
  { id: "settings", icon: "⚙", label: "设置与备份" },
];

function initialView(): ViewId {
  const requested = new URLSearchParams(window.location.search).get("view") as ViewId | null;
  return requested && ["discovery", "search", "library", "favorites", "playlist", "sources", "settings", "now-playing"].includes(requested)
    ? requested
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
  return artwork ? (
    <img className={`cover ${className}`} src={artwork} alt={`${title} 封面`} crossOrigin="anonymous" />
  ) : (
    <div className={`cover cover-placeholder ${className}`} aria-label={`${title} 暂无封面`}>
      {initials(title)}
    </div>
  );
}

function App() {
  const [snapshot, setSnapshot] = useState<EngineSnapshot>(EMPTY_ENGINE);
  const [view, setView] = useState<ViewId>(initialView);
  const [viewHistory, setViewHistory] = useState<ViewId[]>([]);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [message, setMessage] = useState("");
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
  const [sourceUrl, setSourceUrl] = useState("");
  const [configSource, setConfigSource] = useState<ListedSource | null>(null);
  const [sourceConfigDraft, setSourceConfigDraft] = useState<SourceConfigDraft | null>(null);
  const [sourceConfigRevealed, setSourceConfigRevealed] = useState(false);
  const [sourceConfigBusy, setSourceConfigBusy] = useState(false);
  const [backupText, setBackupText] = useState("");
  const [cacheStatus, setCacheStatus] = useState<CacheStatus | null>(null);
  const [cacheLimitGiB, setCacheLimitGiB] = useState("5");
  const [onlineFavorites, setOnlineFavorites] = useState<CatalogTrack[]>([]);

  const [searchQuery, setSearchQuery] = useState("");
  const [searchState, setSearchState] = useState<SearchState>("idle");
  const [suggestions, setSuggestions] = useState<CatalogTrack[]>([]);
  const [searchResults, setSearchResults] = useState<CatalogTrack[]>([]);
  const [chartTracks, setChartTracks] = useState<CatalogTrack[]>([]);
  const [suggestionOpen, setSuggestionOpen] = useState(false);
  const [suggestionIndex, setSuggestionIndex] = useState(-1);
  const [playingCatalogKey, setPlayingCatalogKey] = useState<string | null>(null);
  const searchRequest = useRef<AbortController | null>(null);

  const [selectedCatalogTrack, setSelectedCatalogTrack] = useState<CatalogTrack | null>(null);
  const [lyrics, setLyrics] = useState<LyricDocument | null>(null);
  const lyricRefs = useRef<Array<HTMLParagraphElement | null>>([]);

  /** Logical playlist (local paths + online CatalogTrack metadata). Online never pre-resolved. */
  const [playlist, setPlaylist] = useState<PlaylistEntry[]>([]);
  const [playlistIndex, setPlaylistIndex] = useState<number | null>(null);
  const [queuePanelOpen, setQueuePanelOpen] = useState(false);
  const shufflePlayedRef = useRef<Set<number>>(new Set());
  const advancingRef = useRef(false);
  const playlistRef = useRef(playlist);
  const playlistIndexRef = useRef(playlistIndex);
  const snapshotRef = useRef(snapshot);
  playlistRef.current = playlist;
  playlistIndexRef.current = playlistIndex;
  snapshotRef.current = snapshot;

  const run = async <T,>(command: string, args?: Record<string, unknown>): Promise<T | undefined> => {
    try {
      const result = await invoke<T>(command, args);
      setMessage("");
      return result;
    } catch (error) {
      setMessage(String(error));
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
    const [nextSources, nextRuntime] = await Promise.all([
      invoke<ListedSource[]>("source_list"),
      invoke<RuntimeStatus>("source_status"),
    ]);
    setSources(nextSources);
    setRuntime(nextRuntime);
  };

  const refreshCache = async () => {
    const [status, favoriteTracks] = await Promise.all([
      invoke<CacheStatus>("cache_status"),
      invoke<CatalogTrack[]>("cache_online_favorites"),
    ]);
    setCacheStatus(status);
    setCacheLimitGiB((status.limitBytes / 1024 / 1024 / 1024).toFixed(2).replace(/\.00$/, ""));
    setOnlineFavorites(favoriteTracks);
  };

  useEffect(() => {
    // Window size is set once in Rust (setup) before first show — do not resize here
    // or the app will open at tauri.conf size then jump larger after React mounts.
    void invoke("ui_ready").catch((error) => setMessage(String(error)));
    void refreshLibrary().catch((error) => setMessage(String(error)));
    void refreshSources().catch((error) => setMessage(String(error)));
    void refreshCache().catch((error) => setMessage(String(error)));
    void invoke<string[]>("player_output_devices")
      .then(setOutputDevices)
      .catch((error) => setMessage(String(error)));
    void invoke<CatalogTrack[]>("metadata_chart", { limit: 12 })
      .then(setChartTracks)
      .catch(() => setChartTracks([]));
  }, []);

  useEffect(() => {
    if (view !== "settings") return;
    void refreshCache().catch((error) => setMessage(String(error)));
    const timer = window.setInterval(() => void refreshCache().catch(() => undefined), 2000);
    return () => window.clearInterval(timer);
  }, [view]);

  useEffect(() => {
    let disposed = false;
    const update = async () => {
      try {
        const next = await invoke<EngineSnapshot>("player_snapshot");
        if (!disposed) setSnapshot(next);
      } catch (error) {
        if (!disposed) setMessage(String(error));
      }
    };
    void update();
    const timer = window.setInterval(update, 150);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

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
  const currentArtwork = selectedCatalogTrack?.artworkUrl ?? null;
  const isPlaying = snapshot.status === "playing" || snapshot.status === "loading";
  const shownPosition = dragPosition ?? pendingSeek?.target ?? snapshot.positionSeconds;
  const shownVolume = volumeDraft ?? snapshot.volume;
  const measuredSourceSpec = formatSourceSpec(snapshot);
  const suspiciousQuality = isSuspiciousQuality(currentQuality, snapshot);
  const selectedOnlineFavorite = selectedCatalogTrack
    ? onlineFavorites.some((track) => track.providerId === selectedCatalogTrack.providerId && track.providerTrackId === selectedCatalogTrack.providerTrackId)
    : false;
  const activeSource = sources.find((source) => source.id === runtime?.activeSourceId || source.active) ?? null;
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
    const query = searchQuery.trim();
    searchRequest.current?.abort();
    if (!query) {
      setSuggestions([]);
      setSearchState("idle");
      setSuggestionOpen(false);
      return;
    }
    const controller = new AbortController();
    searchRequest.current = controller;
    setSearchState("loading");
    const timer = window.setTimeout(async () => {
      try {
        const tracks = await invoke<CatalogTrack[]>("metadata_search", { query, limit: 9 });
        if (controller.signal.aborted) return;
        setSuggestions(tracks);
        setSearchState(tracks.length ? "ready" : "empty");
        setSuggestionOpen(true);
        setSuggestionIndex(-1);
      } catch (error) {
        if (!controller.signal.aborted) {
          setSearchState("error");
          setMessage(String(error));
        }
      }
    }, 200);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [searchQuery]);

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

  // Local-only playlists: engine owns advancement — mirror queueIndex into playlistIndex.
  useEffect(() => {
    if (!playlistIsLocalOnly(playlist)) return;
    if (snapshot.queueIndex !== null && snapshot.queueIndex !== playlistIndex) {
      setPlaylistIndex(snapshot.queueIndex);
    }
  }, [snapshot.queueIndex, playlist, playlistIndex]);

  const artists = useMemo(
    () => [...new Set(suggestions.map((track) => track.artist).filter(Boolean))].slice(0, 2),
    [suggestions],
  );
  const albums = useMemo(
    () => [...new Set(suggestions.map((track) => track.album).filter(Boolean))].slice(0, 2),
    [suggestions],
  );

  const loadLyricsFor = async (title: string, artist: string, durationMs: number | null, baseMessage: string) => {
    try {
      const lyricDocument = await invoke<LyricDocument | null>("metadata_lyrics", {
        title,
        artist,
        durationMs,
      });
      setLyrics(lyricDocument);
    } catch (lyricError) {
      setMessage(`${baseMessage} 歌曲已播放，但歌词加载失败：${String(lyricError)}`);
    }
  };

  /**
   * Resolve and play a single online CatalogTrack into the engine.
   * Constraint 2: only called when the playhead actually reaches this track — never batch.
   */
  const resolveAndPlayOnline = async (
    wanted: CatalogTrack,
    quality: QualityPreference,
    opts?: { allowPreviewFallback?: boolean; candidates?: CatalogTrack[] },
  ): Promise<OnlinePlaybackResult | null> => {
    const key = catalogKey(wanted);
    console.info("[GXPlayer] online resolve request", { key, title: wanted.title, quality });
    try {
      const online = await invoke<OnlinePlaybackResult>("player_play_online_track", {
        track: wanted,
        quality: quality === "auto" ? null : quality,
        sourceId: null,
      });
      console.info("[GXPlayer] online resolve ok", {
        key,
        cacheHit: online.cacheHit,
        quality: online.quality,
      });
      setSelectedCatalogTrack(online.track);
      setCurrentQuality(online.quality);
      setLyrics(null);
      const sourceLabel = online.sourceName || activeSource?.metadata.name || "当前 LX 音源";
      const playbackMessage = online.cacheHit
        ? `已命中本地缓存 · ${online.quality ?? "自动"}，无需再次请求音频直链。`
        : `${sourceLabel} 已解析整首播放${online.quality ? ` · ${online.quality}` : ""}，本次播放会顺手写入缓存。`;
      setMessage(playbackMessage);
      void loadLyricsFor(online.track.title, online.track.artist, online.track.durationMs, playbackMessage);
      return online;
    } catch (onlineError) {
      console.warn("[GXPlayer] online resolve failed", { key, error: String(onlineError) });
      if (!opts?.allowPreviewFallback) {
        setMessage(`《${wanted.title}》解析失败，已跳过：${String(onlineError)}`);
        return null;
      }
      try {
        const preview = await invoke<{ track: CatalogTrack; replacedProviderId: string | null }>("metadata_play_preview", {
          wanted,
          candidates: opts.candidates ?? [wanted],
        });
        setSelectedCatalogTrack(preview.track);
        setCurrentQuality("preview");
        setLyrics(null);
        const playbackMessage = `LX 整首解析失败，已回退为 ${preview.track.providerId} 官方 30 秒预览。原因：${String(onlineError)}`;
        setMessage(playbackMessage);
        void loadLyricsFor(preview.track.title, preview.track.artist, preview.track.durationMs, playbackMessage);
        return {
          track: preview.track,
          sourceId: null,
          sourceName: null,
          quality: "preview",
          cacheHit: false,
        };
      } catch (previewError) {
        setMessage(`《${wanted.title}》播放失败：${String(onlineError)}；预览也失败：${String(previewError)}`);
        return null;
      }
    }
  };

  const playPlaylistEntry = async (entries: PlaylistEntry[], index: number, opts?: { allowPreviewFallback?: boolean }) => {
    const entry = entries[index];
    if (!entry) return;
    if (entry.kind === "local") {
      if (playlistIsLocalOnly(entries)) {
        const paths = entries.map((item) => (item as Extract<PlaylistEntry, { kind: "local" }>).path);
        await invoke("player_load_local", { paths, startIndex: index });
      } else {
        await invoke("player_load_local", { paths: [entry.path], startIndex: 0 });
      }
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      setLyrics(null);
      return;
    }
    const result = await resolveAndPlayOnline(entry.track, entry.quality, {
      allowPreviewFallback: opts?.allowPreviewFallback,
      candidates: entries.filter((item): item is Extract<PlaylistEntry, { kind: "online" }> => item.kind === "online").map((item) => item.track),
    });
    if (!result) {
      // Skip failed online track and continue.
      await advanceFromIndex(entries, index, "ended");
    }
  };

  const advanceFromIndex = async (
    entries: PlaylistEntry[],
    current: number,
    intent: "ended" | "next" | "previous",
  ) => {
    if (advancingRef.current) return;
    advancingRef.current = true;
    try {
      const mode = snapshotRef.current.playMode ?? "sequential";
      // Skip chain for failed resolves — walk until success or exhaustion (cap to list length).
      let cursor = current;
      for (let attempt = 0; attempt < entries.length; attempt += 1) {
        const next = pickPlaylistIndex(mode, cursor, entries.length, attempt === 0 ? intent : "ended", shufflePlayedRef.current);
        if (next === null) {
          setPlaylistIndex(cursor);
          return;
        }
        setPlaylistIndex(next);
        const entry = entries[next];
        if (!entry) return;
        if (entry.kind === "local") {
          await invoke("player_load_local", { paths: [entry.path], startIndex: 0 });
          setSelectedCatalogTrack(null);
          setCurrentQuality(null);
          setLyrics(null);
          return;
        }
        const key = catalogKey(entry.track);
        setPlayingCatalogKey(key);
        const result = await resolveAndPlayOnline(entry.track, entry.quality, { allowPreviewFallback: false });
        setPlayingCatalogKey(null);
        if (result) return;
        // Resolve failed — treat as ended and try the following track.
        cursor = next;
      }
    } finally {
      advancingRef.current = false;
    }
  };

  const replacePlaylist = async (entries: PlaylistEntry[], startIndex: number, opts?: { allowPreviewFallback?: boolean }) => {
    if (!entries.length) return;
    const index = Math.max(0, Math.min(startIndex, entries.length - 1));
    shufflePlayedRef.current = new Set([index]);
    setPlaylist(entries);
    setPlaylistIndex(index);
    const startKey = entries[index]?.kind === "online" ? catalogKey(entries[index]!.track) : null;
    if (startKey) setPlayingCatalogKey(startKey);
    try {
      await playPlaylistEntry(entries, index, opts);
    } finally {
      if (startKey) setPlayingCatalogKey(null);
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
      await invoke("player_load_local", { paths, startIndex: 0 });
      const entries: PlaylistEntry[] = paths.map((path) => {
        const name = path.split(/[/\\]/).pop()?.replace(/\.[^.]+$/, "") || "未命名";
        return { kind: "local", path, title: name, artist: "", durationSeconds: null };
      });
      shufflePlayedRef.current = new Set([0]);
      setPlaylist(entries);
      setPlaylistIndex(0);
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      setLyrics(null);
      await refreshLibrary();
    } catch (error) {
      setMessage(String(error));
    }
  };

  /** Click a local track: load the entire current view as the queue, start at the clicked item. */
  const playLocalInList = async (tracks: LibraryTrack[], track: LibraryTrack) => {
    const startIndex = Math.max(0, tracks.findIndex((item) => item.id === track.id));
    const entries = tracks.map(localEntryFromLibrary);
    try {
      await replacePlaylist(entries, startIndex === -1 ? 0 : startIndex);
    } catch (error) {
      setMessage(String(error));
    }
  };

  const enqueueLocalTracks = async (tracks: LibraryTrack[]) => {
    if (!tracks.length) return;
    const paths = tracks.map((track) => track.path);
    try {
      await invoke("player_enqueue_local", { paths });
      setPlaylist((prev) => [...prev, ...tracks.map(localEntryFromLibrary)]);
      setMessage(`已添加 ${tracks.length} 首到队列`);
    } catch (error) {
      setMessage(String(error));
    }
  };

  /** Click a catalog track: queue the whole list as online placeholders; resolve only the clicked one. */
  const playCatalogInList = async (tracks: CatalogTrack[], wanted: CatalogTrack) => {
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
    await replacePlaylist(entries, startIndex, { allowPreviewFallback: true });
    navigateTo("now-playing");
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
    if (playingCatalogKey) return;
    await playCatalogInList(context, wanted);
  };

  const enqueueCatalogTracks = (tracks: CatalogTrack[]) => {
    if (!tracks.length) return;
    console.info("[GXPlayer] online enqueue metadata only", { count: tracks.length });
    setPlaylist((prev) => [
      ...prev,
      ...tracks.map((track) => onlineEntryFromCatalog(track, qualityPreference)),
    ]);
    setMessage(`已添加 ${tracks.length} 首在线歌曲到队列（播放到时再解析）`);
  };

  const cyclePlayMode = async () => {
    const current = snapshot.playMode ?? "sequential";
    const index = PLAY_MODE_ORDER.indexOf(current);
    const next = PLAY_MODE_ORDER[(index + 1) % PLAY_MODE_ORDER.length] ?? "sequential";
    if (next === "shuffle") shufflePlayedRef.current = new Set(playlistIndex !== null ? [playlistIndex] : []);
    setSnapshot((state) => ({ ...state, playMode: next }));
    await run("player_set_play_mode", { mode: next });
  };

  const handleTransportNext = async () => {
    const entries = playlistRef.current;
    if (entries.length && !playlistIsLocalOnly(entries)) {
      const current = playlistIndexRef.current ?? 0;
      await advanceFromIndex(entries, current, "next");
      return;
    }
    await run("player_next");
  };

  const handleTransportPrevious = async () => {
    const entries = playlistRef.current;
    if (entries.length && !playlistIsLocalOnly(entries)) {
      const current = playlistIndexRef.current ?? 0;
      await advanceFromIndex(entries, current, "previous");
      return;
    }
    await run("player_previous");
  };

  const jumpToPlaylistIndex = async (index: number) => {
    const entries = playlistRef.current;
    const target = entries[index];
    if (!target) return;
    if (playlistIsLocalOnly(entries)) {
      await run("player_jump", { index });
      setPlaylistIndex(index);
      setSelectedCatalogTrack(null);
      return;
    }
    shufflePlayedRef.current.add(index);
    setPlaylistIndex(index);
    const key = target.kind === "online" ? catalogKey(target.track) : null;
    if (key) setPlayingCatalogKey(key);
    try {
      await playPlaylistEntry(entries, index);
    } finally {
      if (key) setPlayingCatalogKey(null);
    }
  };

  const removePlaylistIndex = async (index: number) => {
    const previous = playlistRef.current;
    const entries = [...previous];
    if (index < 0 || index >= entries.length) return;
    const current = playlistIndexRef.current;
    const removedCurrent = current === index;
    const wasLocalOnly = playlistIsLocalOnly(previous);
    entries.splice(index, 1);
    // Remap shuffle played indices after mid-cycle edits.
    const nextPlayed = new Set<number>();
    shufflePlayedRef.current.forEach((value) => {
      if (value < index) nextPlayed.add(value);
      else if (value > index) nextPlayed.add(value - 1);
    });
    shufflePlayedRef.current = nextPlayed;

    if (wasLocalOnly) {
      await run("player_remove_queue_item", { index });
    }

    setPlaylist(entries);
    if (!entries.length) {
      setPlaylistIndex(null);
      if (!wasLocalOnly) await run("player_clear_queue");
      return;
    }
    let nextIndex: number | null = current;
    if (current === null) nextIndex = null;
    else if (current > index) nextIndex = current - 1;
    else if (current === index) nextIndex = Math.min(index, entries.length - 1);
    setPlaylistIndex(nextIndex);
    if (removedCurrent && nextIndex !== null && !playlistIsLocalOnly(entries)) {
      await playPlaylistEntry(entries, nextIndex);
    }
  };

  const clearPlaylist = async () => {
    setPlaylist([]);
    setPlaylistIndex(null);
    shufflePlayedRef.current.clear();
    await run("player_clear_queue");
    setMessage("队列已清空");
  };

  // Online/mixed playlists: engine holds only the current resolved track.
  // When it naturally ends (stopped), advance and resolve the next online item on demand.
  const prevStatusRef = useRef(snapshot.status);
  useEffect(() => {
    const prev = prevStatusRef.current;
    prevStatusRef.current = snapshot.status;
    if (snapshot.status !== "stopped") return;
    if (prev === "stopped" || prev === "idle") return;
    const entries = playlistRef.current;
    if (!entries.length || playlistIsLocalOnly(entries)) return;
    const current = playlistIndexRef.current ?? 0;
    void advanceFromIndex(entries, current, "ended");
  }, [snapshot.status]);

  const switchOnlineQuality = async (preference: QualityPreference) => {
    if (!selectedCatalogTrack || !currentQueueItem?.online || qualitySwitching) return;
    setQualitySwitching(true);
    try {
      const online = await invoke<OnlinePlaybackResult>("player_play_online_track", {
        track: selectedCatalogTrack,
        quality: preference === "auto" ? null : preference,
        sourceId: null,
      });
      setSelectedCatalogTrack(online.track);
      setCurrentQuality(online.quality);
      setMessage(online.cacheHit ? `已切换到本地缓存 ${online.quality ?? "自动"}。` : `已切换到 ${online.quality ?? "自动"}，并重新开始流式播放。`);
    } catch (error) {
      setMessage(`切换音质失败，已保留当前播放：${String(error)}`);
    } finally {
      setQualitySwitching(false);
    }
  };

  const updateQualityPreference = (preference: QualityPreference) => {
    setQualityPreference(preference);
    window.localStorage.setItem("gxplayer.defaultQuality", preference);
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
      setMessage(String(error));
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
      setMessage(String(error));
    } finally {
      setSourceConfigBusy(false);
    }
  };

  const submitSearch = async (queryOverride?: string) => {
    const query = (queryOverride ?? searchQuery).trim();
    if (!query) return;
    setSuggestionOpen(false);
    navigateTo("search");
    setSearchState("loading");
    const results = await run<CatalogTrack[]>("metadata_search", { query, limit: 40 });
    if (results !== undefined) {
      setSearchResults(results);
      setSearchState(results.length ? "ready" : "empty");
    }
  };

  const onSearchKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      setSuggestionOpen(false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setSuggestionOpen(true);
      setSuggestionIndex((index) => Math.max(-1, Math.min(suggestions.length - 1, index + direction)));
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const selected = suggestions[suggestionIndex];
      if (selected) void playCatalog(selected);
      else void submitSearch();
    }
  };

  const setAudioMode = async (mode: AudioMode) => {
    setSnapshot((state) => ({ ...state, audioMode: mode }));
    await run("player_set_audio_mode", { mode });
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
      setMessage("缓存上限必须是正数。");
      return;
    }
    const status = await invoke<CacheStatus>("cache_set_limit", { limitBytes: Math.round(gib * 1024 * 1024 * 1024) });
    setCacheStatus(status);
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
      setMessage(String(error));
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
      setMessage(String(error));
    }
  };

  const renderTrackRow = (track: LibraryTrack, index: number, list: LibraryTrack[], playlistId?: number) => (
        <div className="track-row" role="listitem" key={track.id}>
          <button className="track-main" onClick={() => void playLocalInList(list, track)}>
            <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span>
              <strong>{track.title}</strong>
              <small>{track.artist || "未知歌手"}{track.album ? ` · ${track.album}` : ""}</small>
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
          <button className="catalog-card" disabled={playingCatalogKey !== null} aria-busy={resolving} onClick={() => void playCatalogInList(tracks, track)}>
            <Cover artwork={track.artworkUrl} title={track.title} />
            <strong>{track.title}</strong>
            <span>{track.artist}</span>
            <small>{resolving ? "正在解析整首播放…" : track.album || track.providerId}</small>
            <i aria-hidden="true">{resolving ? "…" : "▶"}</i>
          </button>
          <button
            type="button"
            className="catalog-enqueue"
            disabled={playingCatalogKey !== null}
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
        {chartTracks.length > 0 && <section className="section-block panel-enter delay-2"><div className="section-heading"><div><p className="eyebrow">DISCOVER</p><h2>正在流行</h2></div><button onClick={() => { setSearchResults(chartTracks); setSearchQuery("中国区热门"); navigateTo("search"); }}>查看全部 →</button></div>{renderCatalogRows(chartTracks.slice(0, 6))}</section>}
      </div>
    );

    if (view === "search") return (
      <div className="page">
        <PageHeading eyebrow="SEARCH" title={searchQuery ? `“${searchQuery}” 的结果` : "搜索音乐"} copy={runtime?.state === "ready" ? `${sourceStatus.copy} 点击歌曲将优先解析整首播放，失败时会明确提示并回退官方 30 秒预览。` : `${sourceStatus.title}：${sourceStatus.copy} 当前仍可尝试官方 30 秒预览。`} />
        {searchState === "loading" ? <LoadingState /> : searchResults.length ? renderCatalogRows(searchResults) : <EmptyState title="从顶栏开始搜索" copy="输入歌名、歌手或专辑，联想结果会按类型分组。" />}
      </div>
    );

    if (view === "library" || view === "favorites") {
      const tracks = view === "library" ? library : favorites;
      return <div className="page"><PageHeading eyebrow={view === "library" ? "LOCAL LIBRARY" : "FAVORITES"} title={view === "library" ? "本地曲库" : "我的收藏"} copy={view === "library" ? `${library.length} 首本地音乐，播放不经过 WebView。` : `${tracks.length + onlineFavorites.length} 首收藏；在线收藏的缓存会被钉住。`} action={view === "library" ? <button className="primary" onClick={chooseFiles}>导入音乐</button> : undefined} />{view === "favorites" && onlineFavorites.length > 0 && <section className="section-block"><div className="section-heading"><div><h3>在线收藏</h3><p>尚未缓存的歌曲不会主动下载，会等你自然播放。</p></div></div>{renderCatalogRows(onlineFavorites)}</section>}{tracks.length ? <section className="section-block"><div className="section-heading"><div><h3>{view === "library" ? "本地音乐" : "本地收藏"}</h3></div></div>{renderTrackRows(tracks)}</section> : view === "library" || onlineFavorites.length === 0 ? <EmptyState title={view === "library" ? "还没有本地音乐" : "还没有收藏"} copy={view === "library" ? "选择音频文件，它们会自动进入曲库。" : "播放在线歌曲或打开本地曲库，点一下心形即可收藏。"} action={view === "library" ? "选择音乐" : undefined} onAction={view === "library" ? chooseFiles : undefined} /> : null}</div>;
    }

    if (view === "playlist") return (
      <div className="page"><PageHeading eyebrow="PLAYLIST" title={activePlaylist?.name ?? "歌单"} copy={`${playlistTracks.length} 首音乐`} action={activePlaylist ? <button className="danger" onClick={async () => { await run("library_delete_playlist", { playlistId: activePlaylist.id }); navigateTo("discovery"); setActivePlaylist(null); await refreshLibrary(); }}>删除歌单</button> : undefined} />{playlistTracks.length && activePlaylist ? renderTrackRows(playlistTracks, activePlaylist.id) : <EmptyState title="这个歌单还没有歌" copy="回到曲库，把想听的歌加进来。" action="去曲库" onAction={() => navigateTo("library")} />}</div>
    );

    if (view === "sources") return (
      <div className="page"><PageHeading eyebrow="MUSIC SOURCES" title="管理音源" copy="音源脚本运行在独立沙箱中；程序启动时也会自动扫描 %APPDATA%\\com.gxplayer.desktop\\sources\\drop-in 里的 .js。" action={<button onClick={async () => { const selected = await open({ multiple: false, filters: [{ name: "LX 音源脚本", extensions: ["js"] }] }); if (selected && !Array.isArray(selected)) { await run("source_import_file", { path: selected }); await refreshSources(); } }}>导入脚本</button>} />
        <section className="source-status-card"><span className={`runtime-dot ${runtime?.state ?? "no_source"}`} /><div><strong>{sourceStatus.title}</strong><p>{sourceStatus.copy}</p></div><code>GEN {runtime?.generation ?? 0}</code></section>
        <div className="inline-form"><input aria-label="音源脚本 URL" placeholder="https://…/source.js" value={sourceUrl} onChange={(event) => setSourceUrl(event.target.value)} /><button className="primary" disabled={!sourceUrl.trim()} onClick={async () => { await run("source_import_url", { url: sourceUrl.trim() }); setSourceUrl(""); await refreshSources(); }}>从 URL 导入</button></div>
        <div className="source-list">{sources.map((source) => <article className={`source-card ${source.active ? "active" : ""}`} key={source.id}><div><span className="source-badge">{source.active ? "正在使用" : source.hasConfig ? "已配置" : "可用"}</span><h3>{source.metadata.name || "未命名音源"}</h3><p>{source.metadata.author || "未知作者"} · v{source.metadata.version || "?"}</p></div><div className="source-actions"><label><input type="checkbox" checked={source.updatesEnabled} onChange={async (event) => { await run("source_set_updates_enabled", { id: source.id, enabled: event.target.checked }); await refreshSources(); }} /> 更新提醒</label><button disabled={sourceConfigBusy} onClick={() => void openSourceConfig(source)}>配置</button><button disabled={source.active} onClick={async () => { await run("source_activate", { id: source.id }); await refreshSources(); }}>启用</button><button className="danger" onClick={async () => { await run("source_remove", { id: source.id }); await refreshSources(); }}>删除</button></div></article>)}</div>
      </div>
    );

    if (view === "settings") return (
      <div className="page"><PageHeading eyebrow="SETTINGS" title="设置与备份" copy="输出设备、音效模式和本地数据都在这里管理。" />
        <div className="settings-grid"><section className="settings-card"><h3>输出设备</h3><p>切换时会从当前位置继续播放。</p><select value={snapshot.outputDevice ?? ""} onChange={(event) => void run("player_set_output_device", { name: event.target.value || null })}><option value="">系统默认设备</option>{outputDevices.map((device) => <option key={device} value={device}>{device}</option>)}</select></section>
        <section className="settings-card"><h3>默认音质</h3><p>自动会按当前平台能力从高到低尝试，并在解析失败时逐档回退。</p><select value={qualityPreference} onChange={(event) => updateQualityPreference(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></section>
        <section className="settings-card"><h3>默认听感</h3><p>音乐模式保持 DSP 透明旁路；影院/游戏模式启用空间处理。</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></section>
        <section className="settings-card cache-settings"><h3>在线播放缓存</h3><p>只保存自然播放时已经收到的字节，不会预抓或批量下载。</p><dl><div><dt>当前占用</dt><dd>{cacheStatus ? `${formatBytes(cacheStatus.totalBytes)} · ${cacheStatus.entryCount} 项` : "读取中…"}</dd></div><div><dt>收藏钉住</dt><dd>{cacheStatus?.pinnedCount ?? 0} 项</dd></div><div><dt>目录</dt><dd title={cacheStatus?.directory}>{cacheStatus?.directory ?? "读取中…"}</dd></div></dl><label><span>上限（GiB）</span><div className="inline-form"><input type="number" min="0.125" step="0.5" value={cacheLimitGiB} onChange={(event) => setCacheLimitGiB(event.target.value)} /><button onClick={() => void saveCacheLimit()}>保存</button></div></label><div className="cache-actions"><button onClick={() => void chooseCacheDirectory()}>选择目录</button><button onClick={async () => { const status = await invoke<CacheStatus>("cache_reset_directory"); setCacheStatus(status); setMessage("已恢复默认缓存目录；旧目录内容未迁移。"); }}>恢复默认</button><button onClick={async () => { const status = await invoke<CacheStatus>("cache_clear", { includePinned: false }); setCacheStatus(status); }}>清未收藏</button><button className="danger" onClick={async () => { const status = await invoke<CacheStatus>("cache_clear", { includePinned: true }); setCacheStatus(status); }}>清空全部</button></div></section></div>
        <section className="backup-card"><div className="section-heading"><div><h3>配置备份</h3><p>包含本地曲库、歌单、音源脚本及音源密钥；备份内容请勿公开。</p></div><div><button onClick={() => void exportBackup()}>生成备份</button><button className="primary" disabled={!backupText.trim()} onClick={() => void restoreBackup()}>恢复备份</button></div></div><textarea aria-label="GXPlayer 备份 JSON" placeholder="生成的备份会显示在这里，也可以粘贴已有备份。" value={backupText} onChange={(event) => setBackupText(event.target.value)} /></section>
      </div>
    );

    return (
      <div className="page now-playing-page">
        <div className="now-grid">
          <section className="record-column"><div className={`record ${isPlaying ? "spinning" : ""}`}><Cover artwork={currentArtwork} title={currentTitle} className="record-cover" /><span className="record-hole" /></div><p className="eyebrow">NOW PLAYING</p><h1>{currentTitle}</h1><p className="artist-line">{currentArtist}</p>{measuredSourceSpec && <p className={`source-spec ${suspiciousQuality ? "suspicious" : ""}`}>{currentQuality && currentQueueItem?.online ? <><span>{currentQuality}（自报）</span><b>·</b></> : null}<span>实测 {measuredSourceSpec}</span>{suspiciousQuality && <em title="自报高解析音质与解码规格不一致">⚠ 疑似虚标</em>}</p>}</section>
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
                      <small>{entryArtist(entry)}{entry.kind === "online" ? " · 待解析" : ""}</small>
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
    <div className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""}`} style={{ "--accent": accent } as CSSProperties}>
      <div className="ambient-light" aria-hidden="true" />
      <div className="ambient-light ambient-light-secondary" aria-hidden="true" />
      <div className="shell-noise" aria-hidden="true" />
      <header className="top-bar" data-tauri-drag-region>
        <div className="brand-cluster">
          <button className="menu-button" onClick={() => setSidebarCollapsed((value) => !value)} aria-label={sidebarCollapsed ? "展开侧栏" : "收起侧栏"}>☰</button>
          <button className="logo" onClick={() => navigateTo("discovery")} aria-label="返回探索页"><img src={gxplayerIcon} alt="" /></button>
          <button className="history-back" onClick={navigateBack} disabled={!viewHistory.length} aria-label="返回上一页" title="返回上一页">‹</button>
        </div>
        <div className="global-search">
          <span aria-hidden="true">⌕</span>
          <input aria-label="搜索歌曲、歌手、专辑" placeholder="搜索歌曲、歌手、专辑…" value={searchQuery} onChange={(event) => setSearchQuery(event.target.value)} onFocus={() => searchQuery.trim() && setSuggestionOpen(true)} onKeyDown={onSearchKeyDown} />
          {searchState === "loading" && <i className="search-spinner" aria-label="正在搜索" />}
          {suggestionOpen && <div className="suggestions" role="listbox">
            {searchState === "empty" && <div className="suggestion-state">没有找到相关音乐</div>}
            {suggestions.slice(0, 4).length > 0 && <SuggestionGroup label="歌曲">{suggestions.slice(0, 4).map((track, index) => { const trackKey = `${track.providerId}:${track.providerTrackId}`; const resolving = playingCatalogKey === trackKey; return <button role="option" aria-selected={index === suggestionIndex} aria-busy={resolving} disabled={playingCatalogKey !== null} className={index === suggestionIndex ? "selected" : ""} key={trackKey} onMouseDown={(event) => event.preventDefault()} onClick={() => void playCatalog(track)}><span>{resolving ? "…" : "♪"}</span><strong>{track.title}</strong><small>{resolving ? "正在解析整首播放…" : track.artist}</small></button>; })}</SuggestionGroup>}
            {artists.length > 0 && <SuggestionGroup label="歌手">{artists.map((artist) => <button key={artist} onClick={() => { setSearchQuery(artist); void submitSearch(artist); }}><span>●</span><strong>{artist}</strong><small>歌手</small></button>)}</SuggestionGroup>}
            {albums.length > 0 && <SuggestionGroup label="专辑">{albums.map((album) => <button key={album} onClick={() => { setSearchQuery(album); void submitSearch(album); }}><span>◉</span><strong>{album}</strong><small>专辑</small></button>)}</SuggestionGroup>}
            <button className="view-all" onMouseDown={(event) => event.preventDefault()} onClick={() => void submitSearch()}>查看“{searchQuery}”的全部结果 <span>→</span></button>
          </div>}
        </div>
        <div className="top-bar-trail">
          <button className={`mode-pill ${snapshot.audioMode === "cinema_game" ? "active" : ""}`} onClick={() => navigateTo("now-playing")}><span>⊙</span>{snapshot.audioMode === "music" ? "原声" : "空间"}</button>
        </div>
        <div className="window-controls"><button onClick={() => void getCurrentWindow().minimize()} aria-label="最小化">─</button><button onClick={() => void getCurrentWindow().toggleMaximize()} aria-label="最大化">□</button><button className="close" onClick={() => void getCurrentWindow().close()} aria-label="关闭">×</button></div>
      </header>

      <aside className="sidebar">
        <nav>{NAV_ITEMS.map((item) => <button className={view === item.id ? "active" : ""} onClick={() => navigateTo(item.id)} key={item.id} title={item.label}><span>{item.icon}</span><strong>{item.label}</strong></button>)}</nav>
        <div className="sidebar-playlists"><p>歌单</p>{playlists.slice(0, 8).map((playlist) => <button key={playlist.id} className={activePlaylist?.id === playlist.id && view === "playlist" ? "active" : ""} onClick={() => void openPlaylist(playlist)} title={playlist.name}><span>♬</span><strong>{playlist.name}</strong></button>)}</div>
        <div className="engine-health"><i className={snapshot.status === "failed" ? "bad" : ""} /><span><strong>Rust Engine</strong><small>{snapshot.status === "failed" ? "需要处理" : `${snapshot.underrunCallbacks} underrun`}</small></span></div>
      </aside>

      <main className="content">{renderView()}</main>

      {configSource && sourceConfigDraft && <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeSourceConfig(); }}><section className="config-modal" role="dialog" aria-modal="true" aria-label={`${configSource.metadata.name} 音源配置`}><div className="section-heading"><div><p className="eyebrow">SOURCE CONFIG</p><h3>{configSource.metadata.name || "音源配置"}</h3><p>同时支持源码常量 key 与 LX 全局 ls；关闭或保存后敏感值会从界面状态清空。</p></div><button onClick={closeSourceConfig} aria-label="关闭配置">×</button></div><div className="config-fields"><label><span>源码常量名</span><input value={sourceConfigDraft.constName} placeholder="YuNingXi" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, constName: event.target.value })} /></label><label><span>解析 Key</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.keyValue} placeholder="留空则使用音源公益额度" autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, keyValue: event.target.value })} /></label><label><span>ls.api.addr（可选）</span><input value={sourceConfigDraft.apiAddr} placeholder="https://…" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiAddr: event.target.value })} /></label><label><span>ls.api.pass（可选）</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.apiPass} autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiPass: event.target.value })} /></label></div><label className="config-reveal"><input type="checkbox" checked={sourceConfigRevealed} onChange={(event) => setSourceConfigRevealed(event.target.checked)} /> 临时显示敏感字段</label><div className="modal-actions"><button onClick={closeSourceConfig}>取消</button><button className="primary" disabled={sourceConfigBusy} onClick={() => void saveSourceConfig()}>保存并应用</button></div></section></div>}

      {(message || snapshot.error) && <div className="toast" role="status"><span>!</span><p>{snapshot.error ?? message}</p><button onClick={() => setMessage("")} aria-label="关闭提示">×</button></div>}

      <footer className="player-bar">
        <button className="player-track" onClick={() => navigateTo("now-playing")}>
          <Cover artwork={currentArtwork} title={currentTitle} />
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
            <button type="button" className="play-button" onClick={() => void run(isPlaying ? "player_pause" : "player_play")} disabled={!currentQueueItem && !displayPlaylist.length} aria-label={isPlaying ? "暂停" : "播放"}>
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
          {selectedCatalogTrack && currentQueueItem?.online && <select className="quality-select" aria-label="音源自报音质" title={`音源自报档位：${currentQuality ?? "自动"}`} value={QUALITY_OPTIONS.some((option) => option.value === currentQuality) ? currentQuality ?? "auto" : "auto"} disabled={qualitySwitching} onChange={(event) => void switchOnlineQuality(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.value === "auto" ? `自动${currentQuality ? ` · ${currentQuality}` : ""}` : option.label}</option>)}</select>}
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
                void run("player_set_volume", { volume });
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

      {queuePanelOpen && (
        <aside className="queue-panel" aria-label="播放队列">
          <header className="queue-panel-header">
            <div>
              <p className="eyebrow">QUEUE</p>
              <h3>播放队列</h3>
              <small>{displayPlaylist.length ? `${displayPlaylist.length} 首 · ${PLAY_MODE_META[snapshot.playMode ?? "sequential"].label}` : "队列为空"}</small>
            </div>
            <div className="queue-panel-actions">
              <button type="button" disabled={!displayPlaylist.length} onClick={() => void clearPlaylist()}>清空</button>
              <button type="button" onClick={() => setQueuePanelOpen(false)} aria-label="关闭队列">×</button>
            </div>
          </header>
          {displayPlaylist.length === 0 ? (
            <div className="queue-empty">
              <p>还没有歌曲</p>
              <span>在曲库或搜索结果里点一首，会把当前列表整队入列。</span>
            </div>
          ) : (
            <ul className="queue-list">
              {displayPlaylist.map((entry, index) => {
                const active = index === displayIndex;
                return (
                  <li key={entryKey(entry, index)} className={active ? "active" : ""}>
                    <button type="button" className="queue-main" onClick={() => void jumpToPlaylistIndex(index)}>
                      <span className="queue-index">{active ? "♪" : String(index + 1).padStart(2, "0")}</span>
                      <span>
                        <strong>{entryTitle(entry)}</strong>
                        <small>
                          {entryArtist(entry)}
                          {entry.kind === "online" ? " · 在线" : " · 本地"}
                          {entry.kind === "online" && !active ? " · 待解析" : ""}
                        </small>
                      </span>
                    </button>
                    <button type="button" className="icon-button" aria-label="从队列移除" onClick={() => void removePlaylistIndex(index)}>×</button>
                  </li>
                );
              })}
            </ul>
          )}
        </aside>
      )}
    </div>
  );
}

function PageHeading({ eyebrow, title, copy, action }: { eyebrow: string; title: string; copy: string; action?: ReactNode }) {
  return <header className="page-heading panel-enter"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{copy}</p></div>{action}</header>;
}

function EmptyState({ title, copy, action, onAction }: { title: string; copy: string; action?: string; onAction?: () => void }) {
  return <div className="empty-state"><span>♫</span><h3>{title}</h3><p>{copy}</p>{action && <button className="primary" onClick={onAction}>{action}</button>}</div>;
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
