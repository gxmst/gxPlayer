import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { currentMonitor, getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
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
  type PlaylistSummary,
  type RuntimeStatus,
  type ViewId,
} from "./types";

type SearchState = "idle" | "loading" | "ready" | "empty" | "error";
type AudioMode = EngineSnapshot["audioMode"];

const FALLBACK_ACCENT = "#ff5566";
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

function fallbackAccent(key: string): string {
  let hash = 0;
  for (const character of key) hash = (hash * 31 + character.charCodeAt(0)) | 0;
  const hue = Math.abs(hash) % 360;
  return `hsl(${hue} 72% 62%)`;
}

async function accentFromArtwork(url: string | null, key: string): Promise<string> {
  if (!url) return fallbackAccent(key);
  return new Promise((resolve) => {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onload = () => {
      try {
        const canvas = document.createElement("canvas");
        canvas.width = 32;
        canvas.height = 32;
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (!context) return resolve(fallbackAccent(key));
        context.drawImage(image, 0, 0, 32, 32);
        const pixels = context.getImageData(0, 0, 32, 32).data;
        let best = { score: -1, red: 255, green: 85, blue: 102 };
        for (let index = 0; index < pixels.length; index += 16) {
          const red = pixels[index];
          const green = pixels[index + 1];
          const blue = pixels[index + 2];
          const max = Math.max(red, green, blue);
          const min = Math.min(red, green, blue);
          const saturation = max - min;
          const lightness = (max + min) / 2;
          const score = saturation * 1.8 - Math.abs(lightness - 150);
          if (lightness > 55 && score > best.score) best = { score, red, green, blue };
        }
        const lift = Math.max(1, 90 / Math.max(best.red, best.green, best.blue));
        resolve(
          `rgb(${Math.min(255, best.red * lift)} ${Math.min(255, best.green * lift)} ${Math.min(255, best.blue * lift)})`,
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
  const [outputDevices, setOutputDevices] = useState<string[]>([]);

  const [library, setLibrary] = useState<LibraryTrack[]>([]);
  const [favorites, setFavorites] = useState<LibraryTrack[]>([]);
  const [playlists, setPlaylists] = useState<PlaylistSummary[]>([]);
  const [activePlaylist, setActivePlaylist] = useState<PlaylistSummary | null>(null);
  const [playlistTracks, setPlaylistTracks] = useState<LibraryTrack[]>([]);
  const [newPlaylistName, setNewPlaylistName] = useState("");

  const [sources, setSources] = useState<ListedSource[]>([]);
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [sourceUrl, setSourceUrl] = useState("");
  const [backupText, setBackupText] = useState("");

  const [searchQuery, setSearchQuery] = useState("");
  const [searchState, setSearchState] = useState<SearchState>("idle");
  const [suggestions, setSuggestions] = useState<CatalogTrack[]>([]);
  const [searchResults, setSearchResults] = useState<CatalogTrack[]>([]);
  const [chartTracks, setChartTracks] = useState<CatalogTrack[]>([]);
  const [suggestionOpen, setSuggestionOpen] = useState(false);
  const [suggestionIndex, setSuggestionIndex] = useState(-1);
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
    const placeWindow = async () => {
      const monitor = await currentMonitor();
      if (!monitor) return;
      const logicalWidth = monitor.size.width / monitor.scaleFactor;
      const logicalHeight = monitor.size.height / monitor.scaleFactor;
      let width = Math.min(1280, logicalWidth * 0.88);
      let height = width / 1.6;
      const maximumHeight = logicalHeight * 0.86;
      if (height > maximumHeight) {
        height = maximumHeight;
        width = height * 1.6;
      }
      const appWindow = getCurrentWindow();
      await appWindow.setSize(new LogicalSize(Math.floor(width), Math.floor(height)));
      await appWindow.center();
    };
    void placeWindow().catch(() => undefined);
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
  const currentLibraryTrack = useMemo(
    () => library.find((track) => track.path === currentQueueItem?.location) ?? null,
    [currentQueueItem?.location, library],
  );
  const currentTitle = selectedCatalogTrack?.title ?? currentLibraryTrack?.title ?? currentQueueItem?.title ?? "尚未播放";
  const currentArtist = selectedCatalogTrack?.artist ?? currentLibraryTrack?.artist ?? "选择一首歌，让房间亮起来";
  const currentArtwork = selectedCatalogTrack?.artworkUrl ?? null;
  const isPlaying = snapshot.status === "playing" || snapshot.status === "loading";
  const shownPosition = dragPosition ?? snapshot.positionSeconds;

  useEffect(() => {
    let disposed = false;
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
    const loaded = await run("player_load_local", { paths });
    if (loaded !== undefined) {
      setSelectedCatalogTrack(null);
      setLyrics(null);
      await refreshLibrary();
    }
  };

  const playLocal = async (track: LibraryTrack) => {
    const loaded = await run("player_load_local", { paths: [track.path] });
    if (loaded !== undefined) {
      setSelectedCatalogTrack(null);
      setLyrics(null);
    }
  };

  const playCatalog = async (wanted: CatalogTrack) => {
    try {
      const selected = await invoke<{ track: CatalogTrack; replacedProviderId: string | null }>("metadata_play_preview", {
        wanted,
        candidates: searchResults.length ? searchResults : suggestions,
      });
      setSelectedCatalogTrack(selected.track);
      const lyricDocument = await invoke<LyricDocument | null>("metadata_lyrics", {
        title: wanted.title,
        artist: wanted.artist,
        durationMs: wanted.durationMs,
      });
      setLyrics(lyricDocument);
      setView("now-playing");
      setSuggestionOpen(false);
      setMessage(selected.replacedProviderId ? `原平台不可播，已切换到 ${selected.track.providerId}。` : "");
    } catch (error) {
      setMessage(String(error));
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
    setDragPosition(null);
    await run("player_seek", { seconds });
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
      {tracks.map((track) => (
        <button className="catalog-card" onClick={() => void playCatalog(track)} key={`${track.providerId}:${track.providerTrackId}`}>
          <Cover artwork={track.artworkUrl} title={track.title} />
          <strong>{track.title}</strong>
          <span>{track.artist}</span>
          <small>{track.album || track.providerId}</small>
          <i aria-hidden="true">▶</i>
        </button>
      ))}
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
          <div className="mini-stage" aria-label={`当前音效模式：${snapshot.audioMode === "music" ? "原声音乐" : "影院游戏"}`}>
            <span className="stage-listener">你</span>
            <i className="speaker speaker-left" />
            <i className="speaker speaker-right" />
            <div className="stage-orbit" />
            <strong>{snapshot.audioMode === "music" ? "原声" : "空间"}</strong>
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
        <PageHeading eyebrow="SEARCH" title={searchQuery ? `“${searchQuery}” 的结果` : "搜索音乐"} copy="搜索歌曲、歌手或专辑。试听会使用官方预览，并在不可播时寻找跨平台替代。" />
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
      <div className="page"><PageHeading eyebrow="MUSIC SOURCES" title="管理音源" copy="音源脚本只负责解析播放地址，运行在独立沙箱中。" action={<button onClick={async () => { const selected = await open({ multiple: false, filters: [{ name: "LX 音源脚本", extensions: ["js"] }] }); if (selected && !Array.isArray(selected)) { await run("source_import_file", { path: selected }); await refreshSources(); } }}>导入脚本</button>} />
        <section className="source-status-card"><span className={`runtime-dot ${runtime?.state ?? "no_source"}`} /><div><strong>{runtime?.state === "ready" ? "音源已就绪" : "还没有可用音源"}</strong><p>{runtime?.error ?? "导入后可以解析在线歌曲的播放地址。"}</p></div><code>GEN {runtime?.generation ?? 0}</code></section>
        <div className="inline-form"><input aria-label="音源脚本 URL" placeholder="https://…/source.js" value={sourceUrl} onChange={(event) => setSourceUrl(event.target.value)} /><button className="primary" disabled={!sourceUrl.trim()} onClick={async () => { await run("source_import_url", { url: sourceUrl.trim() }); setSourceUrl(""); await refreshSources(); }}>从 URL 导入</button></div>
        <div className="source-list">{sources.map((source) => <article className={`source-card ${source.active ? "active" : ""}`} key={source.id}><div><span className="source-badge">{source.active ? "正在使用" : "可用"}</span><h3>{source.metadata.name || "未命名音源"}</h3><p>{source.metadata.author || "未知作者"} · v{source.metadata.version || "?"}</p></div><div className="source-actions"><label><input type="checkbox" checked={source.updatesEnabled} onChange={async (event) => { await run("source_set_updates_enabled", { id: source.id, enabled: event.target.checked }); await refreshSources(); }} /> 更新提醒</label><button disabled={source.active} onClick={async () => { await run("source_activate", { id: source.id }); await refreshSources(); }}>启用</button><button className="danger" onClick={async () => { await run("source_remove", { id: source.id }); await refreshSources(); }}>删除</button></div></article>)}</div>
      </div>
    );

    if (view === "settings") return (
      <div className="page"><PageHeading eyebrow="SETTINGS" title="设置与备份" copy="输出设备、音效模式和本地数据都在这里管理。" />
        <div className="settings-grid"><section className="settings-card"><h3>输出设备</h3><p>切换时会从当前位置继续播放。</p><select value={snapshot.outputDevice ?? ""} onChange={(event) => void run("player_set_output_device", { name: event.target.value || null })}><option value="">系统默认设备</option>{outputDevices.map((device) => <option key={device} value={device}>{device}</option>)}</select></section>
        <section className="settings-card"><h3>默认听感</h3><p>音乐模式保持 DSP 透明旁路；影院/游戏模式启用空间处理。</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></section></div>
        <section className="backup-card"><div className="section-heading"><div><h3>配置备份</h3><p>包含本地曲库索引、收藏、歌单和音源脚本。</p></div><div><button onClick={() => void exportBackup()}>生成备份</button><button className="primary" disabled={!backupText.trim()} onClick={() => void restoreBackup()}>恢复备份</button></div></div><textarea aria-label="GXPlayer 备份 JSON" placeholder="生成的备份会显示在这里，也可以粘贴已有备份。" value={backupText} onChange={(event) => setBackupText(event.target.value)} /></section>
      </div>
    );

    return (
      <div className="page now-playing-page">
        <div className="now-grid">
          <section className="record-column"><div className={`record ${isPlaying ? "spinning" : ""}`}><Cover artwork={currentArtwork} title={currentTitle} className="record-cover" /><span className="record-hole" /></div><p className="eyebrow">NOW PLAYING</p><h1>{currentTitle}</h1><p className="artist-line">{currentArtist}</p></section>
          <section className="stage-panel"><div className={`sound-stage ${snapshot.audioMode === "music" ? "bypassed" : "enabled"}`} aria-label="声场模式盘"><div className="orbit orbit-one" /><div className="orbit orbit-two" /><span className="listener">你</span><i className="stage-speaker front-left"><b>FL</b></i><i className="stage-speaker front-right"><b>FR</b></i><i className="stage-speaker rear-left"><b>RL</b></i><i className="stage-speaker rear-right"><b>RR</b></i></div><div className="mode-copy"><p className="eyebrow">SOUND MODE</p><h2>{snapshot.audioMode === "music" ? "原声 / 音乐" : "影院 / 游戏"}</h2><p>{snapshot.audioMode === "music" ? "透明直通，不添加空间处理。你的盲测首选。" : "Crossfeed + 立体声 HRTF，仅在需要空间感时开启。"}</p><ModeButtons mode={snapshot.audioMode} onChange={setAudioMode} /></div></section>
        </div>
        <section className="lyrics-panel"><div className="lyrics-scroll">{lyrics?.instrumental ? <p className="lyric active">纯音乐</p> : lyrics?.lines.length ? lyrics.lines.map((line, index) => <p className={`lyric ${index === activeLyricIndex ? "active" : ""}`} key={`${line.timestampMs}-${index}`} ref={(element) => { lyricRefs.current[index] = element; }}>{line.text}</p>) : <div className="lyrics-empty"><strong>歌词会出现在这里</strong><span>在线预览会自动匹配同步歌词。</span></div>}</div></section>
      </div>
    );
  };

  return (
    <div className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""}`} style={{ "--accent": accent } as CSSProperties}>
      <div className="ambient-light" aria-hidden="true" />
      <header className="top-bar" data-tauri-drag-region>
        <button className="menu-button" onClick={() => setSidebarCollapsed((value) => !value)} aria-label={sidebarCollapsed ? "展开侧栏" : "收起侧栏"}>☰</button>
        <button className="logo" onClick={() => setView("discovery")} aria-label="返回探索页"><span>GX</span></button>
        <div className="global-search">
          <span aria-hidden="true">⌕</span>
          <input aria-label="搜索歌曲、歌手、专辑" placeholder="搜索歌曲、歌手、专辑…" value={searchQuery} onChange={(event) => setSearchQuery(event.target.value)} onFocus={() => searchQuery.trim() && setSuggestionOpen(true)} onKeyDown={onSearchKeyDown} />
          {searchState === "loading" && <i className="search-spinner" aria-label="正在搜索" />}
          {suggestionOpen && <div className="suggestions" role="listbox">
            {searchState === "empty" && <div className="suggestion-state">没有找到相关音乐</div>}
            {suggestions.slice(0, 4).length > 0 && <SuggestionGroup label="歌曲">{suggestions.slice(0, 4).map((track, index) => <button role="option" aria-selected={index === suggestionIndex} className={index === suggestionIndex ? "selected" : ""} key={`${track.providerId}:${track.providerTrackId}`} onMouseDown={(event) => event.preventDefault()} onClick={() => void playCatalog(track)}><span>♪</span><strong>{track.title}</strong><small>{track.artist}</small></button>)}</SuggestionGroup>}
            {artists.length > 0 && <SuggestionGroup label="歌手">{artists.map((artist) => <button key={artist} onClick={() => { setSearchQuery(artist); void submitSearch(artist); }}><span>●</span><strong>{artist}</strong><small>歌手</small></button>)}</SuggestionGroup>}
            {albums.length > 0 && <SuggestionGroup label="专辑">{albums.map((album) => <button key={album} onClick={() => { setSearchQuery(album); void submitSearch(album); }}><span>◉</span><strong>{album}</strong><small>专辑</small></button>)}</SuggestionGroup>}
            <button className="view-all" onMouseDown={(event) => event.preventDefault()} onClick={() => void submitSearch()}>查看“{searchQuery}”的全部结果 <span>→</span></button>
          </div>}
        </div>
        <button className={`mode-pill ${snapshot.audioMode === "cinema_game" ? "active" : ""}`} onClick={() => setView("now-playing")}><span>⊙</span>{snapshot.audioMode === "music" ? "原声" : "空间"}</button>
        <div className="window-controls"><button onClick={() => void getCurrentWindow().minimize()} aria-label="最小化">─</button><button onClick={() => void getCurrentWindow().toggleMaximize()} aria-label="最大化">□</button><button className="close" onClick={() => void getCurrentWindow().close()} aria-label="关闭">×</button></div>
      </header>

      <aside className="sidebar">
        <nav>{NAV_ITEMS.map((item) => <button className={view === item.id ? "active" : ""} onClick={() => setView(item.id)} key={item.id} title={item.label}><span>{item.icon}</span><strong>{item.label}</strong></button>)}</nav>
        <div className="sidebar-playlists"><p>歌单</p>{playlists.slice(0, 8).map((playlist) => <button key={playlist.id} className={activePlaylist?.id === playlist.id && view === "playlist" ? "active" : ""} onClick={() => void openPlaylist(playlist)} title={playlist.name}><span>♬</span><strong>{playlist.name}</strong></button>)}</div>
        <div className="engine-health"><i className={snapshot.status === "failed" ? "bad" : ""} /><span><strong>Rust Engine</strong><small>{snapshot.status === "failed" ? "需要处理" : `${snapshot.underrunCallbacks} underrun`}</small></span></div>
      </aside>

      <main className="content">{renderView()}</main>

      {(message || snapshot.error) && <div className="toast" role="status"><span>!</span><p>{snapshot.error ?? message}</p><button onClick={() => setMessage("")} aria-label="关闭提示">×</button></div>}

      <footer className="player-bar">
        <button className="player-track" onClick={() => setView("now-playing")}><Cover artwork={currentArtwork} title={currentTitle} /><span><strong>{currentTitle}</strong><small>{currentArtist}</small></span></button>
        <div className="player-center"><div className="transport"><button onClick={() => void run("player_previous")} aria-label="上一首">◀</button><button className="play-button" onClick={() => void run(isPlaying ? "player_pause" : "player_play")} disabled={!currentQueueItem} aria-label={isPlaying ? "暂停" : "播放"}>{isPlaying ? "Ⅱ" : "▶"}</button><button onClick={() => void run("player_next")} aria-label="下一首">▶</button></div><div className="timeline"><time>{formatTime(shownPosition)}</time><input aria-label="播放进度" type="range" min={0} max={Math.max(snapshot.durationSeconds ?? 0, 0.01)} step={0.05} value={Math.min(shownPosition, Math.max(snapshot.durationSeconds ?? 0, 0.01))} disabled={!currentQueueItem || !snapshot.durationSeconds} onChange={(event) => setDragPosition(Number(event.target.value))} onPointerUp={(event) => void commitSeek(Number(event.currentTarget.value))} onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) void commitSeek(Number(event.currentTarget.value)); }} /><time>{formatTime(snapshot.durationSeconds)}</time></div></div>
        <div className="player-tools"><span aria-hidden="true">♩</span><input aria-label="音量" type="range" min={0} max={1} step={0.01} value={snapshot.volume} onChange={(event) => setSnapshot((state) => ({ ...state, volume: Number(event.target.value) }))} onPointerUp={(event) => void run("player_set_volume", { volume: Number(event.currentTarget.value) })} /><button className={snapshot.audioMode === "cinema_game" ? "active" : ""} onClick={() => void setAudioMode(snapshot.audioMode === "music" ? "cinema_game" : "music")} aria-label="切换音效模式">⊙</button><button onClick={() => setView("settings")} aria-label="更多设置">•••</button></div>
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
  return <div className="mode-buttons" role="radiogroup" aria-label="音效模式"><button role="radio" aria-checked={mode === "music"} className={mode === "music" ? "active" : ""} onClick={() => void onChange("music")}><span>♫</span><strong>原声 / 音乐</strong><small>透明直通</small></button><button role="radio" aria-checked={mode === "cinema_game"} className={mode === "cinema_game" ? "active" : ""} onClick={() => void onChange("cinema_game")}><span>◎</span><strong>影院 / 游戏</strong><small>可选空间处理</small></button></div>;
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
