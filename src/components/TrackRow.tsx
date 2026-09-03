import type { LibraryTrack, PlaylistSummary } from "../types";

type TrackRowProps = {
  track: LibraryTrack;
  index: number;
  selected?: boolean;
  playlistId?: number;
  playlists?: PlaylistSummary[];
  onToggleSelect?: (trackId: number, selected: boolean) => void;
  onPlay: () => void;
  onEnqueue: () => void;
  onToggleFavorite: () => void;
  onAddToPlaylist?: (trackId: number, playlistId: number) => void;
  onRemoveFromPlaylist?: (playlistId: number, trackId: number) => void;
  formatTime: (seconds: number | null) => string;
};

export function TrackRow({
  track,
  index,
  selected = false,
  playlistId,
  playlists,
  onToggleSelect,
  onPlay,
  onEnqueue,
  onToggleFavorite,
  onAddToPlaylist,
  onRemoveFromPlaylist,
  formatTime,
}: TrackRowProps) {
  return (
    <div className="track-row" role="listitem">
      {onToggleSelect && (
        <label className="library-select">
          <input
            type="checkbox"
            checked={selected}
            onChange={(e) => onToggleSelect(track.id, e.target.checked)}
            aria-label={`选择 ${track.title}`}
          />
        </label>
      )}
      <button className="track-main" onClick={onPlay} disabled={Boolean(track.missing)}>
        <span className="track-index">{String(index + 1).padStart(2, "0")}</span>
        <span>
          <strong>
            {track.title}
            {track.missing ? " · 文件缺失" : ""}
          </strong>
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
        onClick={onEnqueue}
        aria-label={track.missing ? `${track.title} 的文件缺失，无法添加到队列` : `将 ${track.title} 添加到队列`}
        title={track.missing ? "文件缺失，无法添加" : "添加到队列"}
        disabled={Boolean(track.missing)}
      >
        ＋
      </button>
      <button
        className={`icon-button ${track.favorite ? "active" : ""}`}
        onClick={onToggleFavorite}
        aria-label={track.favorite ? "取消收藏" : "收藏"}
      >
        {track.favorite ? "♥" : "♡"}
      </button>
      {playlistId && onRemoveFromPlaylist ? (
        <button className="icon-button" aria-label="从歌单移除" onClick={() => onRemoveFromPlaylist(playlistId, track.id)}>
          ×
        </button>
      ) : playlists && onAddToPlaylist ? (
        <select
          aria-label={`将 ${track.title} 添加到歌单`}
          defaultValue=""
          onChange={(e) => {
            const pid = Number(e.target.value);
            if (pid) onAddToPlaylist(track.id, pid);
            e.target.value = "";
          }}
        >
          <option value="">＋ 歌单</option>
          {playlists.map((item) => (
            <option value={item.id} key={item.id}>
              {item.name}
            </option>
          ))}
        </select>
      ) : null}
    </div>
  );
}
