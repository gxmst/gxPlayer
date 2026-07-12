import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import "@fontsource-variable/space-grotesk";
import "@fontsource-variable/noto-sans-sc";
import "@fontsource-variable/jetbrains-mono";
import "./App.css";
import {
  EMPTY_ENGINE,
  type CatalogTrack,
  type EngineSnapshot,
  type LibraryTrack,
  type ListedSource,
  type LyricDocument,
  type OnlinePlaybackResult,
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
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [message, setMessage] = useState("");
  const [accent, setAccent] = useState(FALLBACK_ACCENT);
  const [dragPosition, setDragPosition] = useState<number | null>(null);
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

  useEffect(() => {
    // Window size is set once in Rust (setup) before first show — do not resize here
    // or the app will open at tauri.conf size then jump larger after React mounts.
    void invoke("ui_ready").catch((error) => setMessage(String(error)));
    void refreshLibrary().catch((error) => setMessage(String(error)));
    void refreshSources().catch((error) => setMessage(String(error)));
    void invoke<string[]>("player_output_devices")
      .then(setOutputDevices)
      .catch((error) => setMessage(String(error)));
    void invoke<CatalogTrack[]>("metadata_chart", { limit: 12 })
      .then(setChartTracks)
      .catch(() => setChartTracks([]));
  }, []);

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

  const artists = useMemo(
    () => [...new Set(suggestions.map((track) => track.artist).filter(Boolean))].slice(0, 2),
    [suggestions],
  );
  const albums = useMemo(
    () => [...new Set(suggestions.map((track) => track.album).filter(Boolean))].slice(0, 2),
    [suggestions],
  );

  const chooseFiles = async () => {
    const selected = await open({
      multiple: true,
      directory: false,
      filters: [{ name: "音频", extensions: ["mp3", "flac", "wav", "m4a", "aac", "ogg"] }],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    try {
      await invoke("player_load_local", { paths });
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      setLyrics(null);
      await refreshLibrary();
    } catch (error) {
      setMessage(String(error));
    }
  };

  const playLocal = async (track: LibraryTrack) => {
    try {
      await invoke("player_load_local", { paths: [track.path] });
      setSelectedCatalogTrack(null);
      setCurrentQuality(null);
      setLyrics(null);
    } catch (error) {
      setMessage(String(error));
    }
  };

  const playCatalog = async (wanted: CatalogTrack) => {
    const catalogKey = `${wanted.providerId}:${wanted.providerTrackId}`;
    if (playingCatalogKey) return;
    setPlayingCatalogKey(catalogKey);
    setSuggestionOpen(false);
    try {
      let selectedTrack: CatalogTrack;
      let playbackMessage = "";
      try {
        const online = await invoke<OnlinePlaybackResult>("player_play_online_track", {
          track: wanted,
          quality: qualityPreference === "auto" ? null : qualityPreference,
          sourceId: null,
        });
        selectedTrack = online.track;
        setCurrentQuality(online.quality);
        const sourceLabel = online.sourceName || activeSource?.metadata.name || "当前 LX 音源";
        playbackMessage = `${sourceLabel} 已解析整首播放${online.quality ? ` · ${online.quality}` : ""}。`;
      } catch (onlineError) {
        try {
          const preview = await invoke<{ track: CatalogTrack; replacedProviderId: string | null }>("metadata_play_preview", {
            wanted,
            candidates: searchResults.length ? searchResults : suggestions,
          });
          selectedTrack = preview.track;
          setCurrentQuality("preview");
          playbackMessage = `LX 整首解析失败，已回退为 ${preview.track.providerId} 官方 30 秒预览。原因：${String(onlineError)}`;
        } catch (previewError) {
          throw new Error(`LX 整首播放失败：${String(onlineError)}；官方 30 秒预览也失败：${String(previewError)}`);
        }
      }

      setSelectedCatalogTrack(selectedTrack);
      setLyrics(null);
      setView("now-playing");
      setMessage(playbackMessage);
      try {
        const lyricDocument = await invoke<LyricDocument | null>("metadata_lyrics", {
          title: selectedTrack.title,
          artist: selectedTrack.artist,
          durationMs: selectedTrack.durationMs,
        });
        setLyrics(lyricDocument);
      } catch (lyricError) {
        setMessage(`${playbackMessage} 歌曲已播放，但歌词加载失败：${String(lyricError)}`);
      }
    } catch (error) {
      setMessage(String(error));
    } finally {
      setPlayingCatalogKey(null);
    }
  };

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
      setMessage(`已切换到 ${online.quality ?? "自动"}，并重新开始流式播放。`);
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
    setView("search");
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

  const createPlaylist = async () => {
    if (!newPlaylistName.trim()) return;
    const playlist = await run<PlaylistSummary>("library_create_playlist", { name: newPlaylistName.trim() });
    if (playlist) {
      setNewPlaylistName("");
      await refreshLibrary();
      setActivePlaylist(playlist);
      setPlaylistTracks([]);
      setView("playlist");
    }
  };

  const openPlaylist = async (playlist: PlaylistSummary) => {
    const tracks = await run<LibraryTrack[]>("library_playlist_tracks", { playlistId: playlist.id });
    if (tracks) {
      setActivePlaylist(playlist);
      setPlaylistTracks(tracks);
      setView("playlist");
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

  const renderTrackRow = (track: LibraryTrack, index: number, playlistId?: number) => (
        <div className="track-row" role="listitem" key={track.id}>
          <button className="track-main" onClick={() => void playLocal(track)}>
            <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span>
              <strong>{track.title}</strong>
              <small>{track.artist || "未知歌手"}{track.album ? ` · ${track.album}` : ""}</small>
            </span>
          </button>
          <time>{formatTime(track.durationSeconds)}</time>
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
              {playlists.map((playlist) => <option value={playlist.id} key={playlist.id}>{playlist.name}</option>)}
            </select>
          )}
        </div>
  );

  const renderTrackRows = (tracks: LibraryTrack[], playlistId?: number) =>
    tracks.length > 120 ? (
      <VirtualTrackList tracks={tracks} renderRow={(track, index) => renderTrackRow(track, index, playlistId)} />
    ) : (
      <div className="track-list" role="list">{tracks.map((track, index) => renderTrackRow(track, index, playlistId))}</div>
    );

  const renderCatalogRows = (tracks: CatalogTrack[]) => (
    <div className="catalog-grid">
      {tracks.map((track) => {
        const trackKey = `${track.providerId}:${track.providerTrackId}`;
        const resolving = playingCatalogKey === trackKey;
        return (
        <button className="catalog-card" disabled={playingCatalogKey !== null} aria-busy={resolving} onClick={() => void playCatalog(track)} key={trackKey}>
          <Cover artwork={track.artworkUrl} title={track.title} />
          <strong>{track.title}</strong>
          <span>{track.artist}</span>
          <small>{resolving ? "正在解析整首播放…" : track.album || track.providerId}</small>
          <i aria-hidden="true">{resolving ? "…" : "▶"}</i>
        </button>
      )})}
    </div>
  );

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
              <button onClick={() => setView("now-playing")}>打开播放页</button>
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
          <div className="section-heading"><div><p className="eyebrow">RECENTLY ADDED</p><h2>最近加入</h2></div><button onClick={() => setView("library")}>查看曲库 →</button></div>
          {library.length ? renderTrackRows(library.slice(0, 6)) : <EmptyState title="曲库还是空的" copy="导入熟悉的音乐，从原声模式开始。" action="选择音乐" onAction={chooseFiles} />}
        </section>
        <section className="playlist-strip panel-enter delay-2">
          <div className="section-heading"><div><p className="eyebrow">PLAYLISTS</p><h2>你的歌单</h2></div></div>
          <div className="playlist-cards">
            {playlists.map((playlist) => <button className="playlist-card" key={playlist.id} onClick={() => void openPlaylist(playlist)}><span>♫</span><strong>{playlist.name}</strong><small>{playlist.trackCount} 首</small></button>)}
            <label className="playlist-card create-card"><span>＋</span><input aria-label="新歌单名称" placeholder="新歌单" value={newPlaylistName} onChange={(event) => setNewPlaylistName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void createPlaylist(); }} /><button onClick={() => void createPlaylist()} disabled={!newPlaylistName.trim()}>创建</button></label>
          </div>
        </section>
        {chartTracks.length > 0 && <section className="section-block panel-enter delay-2"><div className="section-heading"><div><p className="eyebrow">DISCOVER</p><h2>正在流行</h2></div><button onClick={() => { setSearchResults(chartTracks); setSearchQuery("中国区热门"); setView("search"); }}>查看全部 →</button></div>{renderCatalogRows(chartTracks.slice(0, 6))}</section>}
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
      return <div className="page"><PageHeading eyebrow={view === "library" ? "LOCAL LIBRARY" : "FAVORITES"} title={view === "library" ? "本地曲库" : "我的收藏"} copy={view === "library" ? `${library.length} 首本地音乐，播放不经过 WebView。` : "留住真正想再听一遍的歌。"} action={view === "library" ? <button className="primary" onClick={chooseFiles}>导入音乐</button> : undefined} />{tracks.length ? renderTrackRows(tracks) : <EmptyState title={view === "library" ? "还没有本地音乐" : "还没有收藏"} copy={view === "library" ? "选择音频文件，它们会自动进入曲库。" : "在曲库里点一下心形，就会出现在这里。"} action={view === "library" ? "选择音乐" : undefined} onAction={view === "library" ? chooseFiles : undefined} />}</div>;
    }

    if (view === "playlist") return (
      <div className="page"><PageHeading eyebrow="PLAYLIST" title={activePlaylist?.name ?? "歌单"} copy={`${playlistTracks.length} 首音乐`} action={activePlaylist ? <button className="danger" onClick={async () => { await run("library_delete_playlist", { playlistId: activePlaylist.id }); setView("discovery"); setActivePlaylist(null); await refreshLibrary(); }}>删除歌单</button> : undefined} />{playlistTracks.length && activePlaylist ? renderTrackRows(playlistTracks, activePlaylist.id) : <EmptyState title="这个歌单还没有歌" copy="回到曲库，把想听的歌加进来。" action="去曲库" onAction={() => setView("library")} />}</div>
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
        <section className="settings-card"><h3>默认听感</h3><p>音乐模式保持 DSP 透明旁路；影院/游戏模式启用空间处理。</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></section></div>
        <section className="backup-card"><div className="section-heading"><div><h3>配置备份</h3><p>包含本地曲库、歌单、音源脚本及音源密钥；备份内容请勿公开。</p></div><div><button onClick={() => void exportBackup()}>生成备份</button><button className="primary" disabled={!backupText.trim()} onClick={() => void restoreBackup()}>恢复备份</button></div></div><textarea aria-label="GXPlayer 备份 JSON" placeholder="生成的备份会显示在这里，也可以粘贴已有备份。" value={backupText} onChange={(event) => setBackupText(event.target.value)} /></section>
      </div>
    );

    return (
      <div className="page now-playing-page">
        <div className="now-grid">
          <section className="record-column"><div className={`record ${isPlaying ? "spinning" : ""}`}><Cover artwork={currentArtwork} title={currentTitle} className="record-cover" /><span className="record-hole" /></div><p className="eyebrow">NOW PLAYING</p><h1>{currentTitle}</h1><p className="artist-line">{currentArtist}</p></section>
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
          <button className="logo" onClick={() => setView("discovery")} aria-label="返回探索页"><span>GX</span></button>
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
          <button className={`mode-pill ${snapshot.audioMode === "cinema_game" ? "active" : ""}`} onClick={() => setView("now-playing")}><span>⊙</span>{snapshot.audioMode === "music" ? "原声" : "空间"}</button>
        </div>
        <div className="window-controls"><button onClick={() => void getCurrentWindow().minimize()} aria-label="最小化">─</button><button onClick={() => void getCurrentWindow().toggleMaximize()} aria-label="最大化">□</button><button className="close" onClick={() => void getCurrentWindow().close()} aria-label="关闭">×</button></div>
      </header>

      <aside className="sidebar">
        <nav>{NAV_ITEMS.map((item) => <button className={view === item.id ? "active" : ""} onClick={() => setView(item.id)} key={item.id} title={item.label}><span>{item.icon}</span><strong>{item.label}</strong></button>)}</nav>
        <div className="sidebar-playlists"><p>歌单</p>{playlists.slice(0, 8).map((playlist) => <button key={playlist.id} className={activePlaylist?.id === playlist.id && view === "playlist" ? "active" : ""} onClick={() => void openPlaylist(playlist)} title={playlist.name}><span>♬</span><strong>{playlist.name}</strong></button>)}</div>
        <div className="engine-health"><i className={snapshot.status === "failed" ? "bad" : ""} /><span><strong>Rust Engine</strong><small>{snapshot.status === "failed" ? "需要处理" : `${snapshot.underrunCallbacks} underrun`}</small></span></div>
      </aside>

      <main className="content">{renderView()}</main>

      {configSource && sourceConfigDraft && <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeSourceConfig(); }}><section className="config-modal" role="dialog" aria-modal="true" aria-label={`${configSource.metadata.name} 音源配置`}><div className="section-heading"><div><p className="eyebrow">SOURCE CONFIG</p><h3>{configSource.metadata.name || "音源配置"}</h3><p>同时支持源码常量 key 与 LX 全局 ls；关闭或保存后敏感值会从界面状态清空。</p></div><button onClick={closeSourceConfig} aria-label="关闭配置">×</button></div><div className="config-fields"><label><span>源码常量名</span><input value={sourceConfigDraft.constName} placeholder="YuNingXi" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, constName: event.target.value })} /></label><label><span>解析 Key</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.keyValue} placeholder="留空则使用音源公益额度" autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, keyValue: event.target.value })} /></label><label><span>ls.api.addr（可选）</span><input value={sourceConfigDraft.apiAddr} placeholder="https://…" autoComplete="off" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiAddr: event.target.value })} /></label><label><span>ls.api.pass（可选）</span><input type={sourceConfigRevealed ? "text" : "password"} value={sourceConfigDraft.apiPass} autoComplete="new-password" onChange={(event) => setSourceConfigDraft({ ...sourceConfigDraft, apiPass: event.target.value })} /></label></div><label className="config-reveal"><input type="checkbox" checked={sourceConfigRevealed} onChange={(event) => setSourceConfigRevealed(event.target.checked)} /> 临时显示敏感字段</label><div className="modal-actions"><button onClick={closeSourceConfig}>取消</button><button className="primary" disabled={sourceConfigBusy} onClick={() => void saveSourceConfig()}>保存并应用</button></div></section></div>}

      {(message || snapshot.error) && <div className="toast" role="status"><span>!</span><p>{snapshot.error ?? message}</p><button onClick={() => setMessage("")} aria-label="关闭提示">×</button></div>}

      <footer className="player-bar">
        <button className="player-track" onClick={() => setView("now-playing")}>
          <Cover artwork={currentArtwork} title={currentTitle} />
          <span>
            <strong>{currentTitle}</strong>
            <small>{currentArtist}</small>
          </span>
        </button>
        <div className="player-center">
          <div className="transport">
            <button type="button" className="transport-btn" onClick={() => void run("player_previous")} aria-label="上一首">
              <span className="glyph-prev" aria-hidden="true" />
            </button>
            <button type="button" className="play-button" onClick={() => void run(isPlaying ? "player_pause" : "player_play")} disabled={!currentQueueItem} aria-label={isPlaying ? "暂停" : "播放"}>
              <span className={isPlaying ? "glyph-pause" : "glyph-play"} aria-hidden="true" />
            </button>
            <button type="button" className="transport-btn" onClick={() => void run("player_next")} aria-label="下一首">
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
          {selectedCatalogTrack && currentQueueItem?.online && <select className="quality-select" aria-label="在线音质" title={`当前音质：${currentQuality ?? "自动"}`} value={QUALITY_OPTIONS.some((option) => option.value === currentQuality) ? currentQuality ?? "auto" : "auto"} disabled={qualitySwitching} onChange={(event) => void switchOnlineQuality(event.target.value as QualityPreference)}>{QUALITY_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.value === "auto" ? `自动${currentQuality ? ` · ${currentQuality}` : ""}` : option.label}</option>)}</select>}
          <div className="volume-cluster">
            <span className="volume-icon" aria-hidden="true" />
            <input
              aria-label="音量"
              type="range"
              className="volume-slider"
              min={0}
              max={1}
              step={0.01}
              value={snapshot.volume}
              style={{ "--fill": `${snapshot.volume * 100}%` } as CSSProperties}
              onChange={(event) => setSnapshot((state) => ({ ...state, volume: Number(event.target.value) }))}
              onPointerUp={(event) => void run("player_set_volume", { volume: Number(event.currentTarget.value) })}
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
          <button type="button" className="tool-btn more-btn" onClick={() => setView("settings")} aria-label="更多设置" title="设置与备份">
            <span className="more-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
          </button>
        </div>
      </footer>
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
