import type { Ref } from "react";
import { Compass, Library, Heart, History, Radio, Settings2, ListMusic, Plus, ArrowUpRight } from "lucide-react";
import type { PlaylistSummary, ViewId } from "../../types";

const navigation = [
  { id: "discovery", label: "探索", icon: Compass },
  { id: "library", label: "曲库", icon: Library },
  { id: "favorites", label: "收藏", icon: Heart },
  { id: "history", label: "播放历史", icon: History },
] as const;

export function Sidebar({ sidebarRef, drawer, view, playlists, activePlaylistId, onNavigate, onOpenPlaylist, onCreatePlaylist }: {
  sidebarRef: Ref<HTMLElement>; drawer: boolean; view: ViewId; playlists: PlaylistSummary[];
  activePlaylistId?: number; onNavigate: (view: ViewId) => void;
  onOpenPlaylist: (playlist: PlaylistSummary) => void; onCreatePlaylist: () => void;
}) {
  return <aside ref={sidebarRef} id="app-sidebar" className={`sidebar ${drawer ? "sidebar-drawer" : ""}`} aria-label="主导航">
    <nav className="primary-navigation" aria-label="音乐">
      <div className="sidebar-group"><span className="sidebar-group-title">音乐空间</span>
        {navigation.map(({ id, label, icon: Icon }) => <button type="button" key={id} title={label} data-tooltip={label}
          className={view === id ? "active" : ""} aria-current={view === id ? "page" : undefined} onClick={() => onNavigate(id)}>
          <span><Icon size={18} /></span><strong>{label}</strong>
        </button>)}
      </div>
    </nav>
    <div className="sidebar-playlists">
      <div className="sidebar-playlist-heading"><span>我的歌单</span><button type="button" className="icon-button" aria-label="新建或导入歌单" title="新建或导入歌单" onClick={onCreatePlaylist}><Plus size={16} /></button></div>
      {playlists.map((playlist) => <button type="button" key={playlist.id} title={playlist.name} data-tooltip={playlist.name}
        className={view === "playlist" && activePlaylistId === playlist.id ? "active" : ""}
        aria-current={view === "playlist" && activePlaylistId === playlist.id ? "page" : undefined} onClick={() => onOpenPlaylist(playlist)}>
        <span><ListMusic size={17} /></span><strong>{playlist.name}</strong><small>{playlist.trackCount}</small>
      </button>)}
      {!playlists.length && <span className="sidebar-empty">还没有歌单</span>}
    </div>
    <nav className="sidebar-system" aria-label="管理">
      <button type="button" title="音源管理" data-tooltip="音源管理" className={view === "sources" ? "active" : ""} onClick={() => onNavigate("sources")}><span><Radio size={18} /></span><strong>音源管理</strong></button>
      <button type="button" title="设置与备份" data-tooltip="设置与备份" className={view === "settings" ? "active" : ""} onClick={() => onNavigate("settings")}><span><Settings2 size={18} /></span><strong>设置</strong></button>
      <button type="button" className="sidebar-player-link" title="正在播放" data-tooltip="正在播放" onClick={() => onNavigate("now-playing")}><span><ArrowUpRight size={18} /></span><strong>正在播放</strong></button>
    </nav>
  </aside>;
}
