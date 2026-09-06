import type { CatalogTrack } from "../types";
import { splitArtistNames } from "../lib/artistNames";
import { ListPlus, LoaderCircle, Play } from "lucide-react";

type CatalogCardProps = {
  track: CatalogTrack;
  resolving?: boolean;
  onPlay: () => void;
  onEnqueue: () => void;
  onSelectArtist: (artist: string) => void;
  Cover: React.ComponentType<{ artwork?: string | null; title: string }>;
};

function ArtistLinks({
  artist,
  onSelect,
  className = "",
  fallback = "未知歌手",
}: {
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
            onClick={(e) => {
              e.stopPropagation();
              onSelect(name);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                e.stopPropagation();
                onSelect(name);
              }
            }}
          >
            {name}
          </span>
        </span>
      ))}
    </span>
  );
}

export function CatalogCard({ track, resolving = false, onPlay, onEnqueue, onSelectArtist, Cover }: CatalogCardProps) {
  return (
    <div className="catalog-card-wrap" role="listitem">
      <div className="catalog-card" aria-busy={resolving}>
        <Cover artwork={track.artworkUrl} title={track.title} />
        <strong>{track.title}</strong>
        <ArtistLinks artist={track.artist} onSelect={onSelectArtist} className="catalog-artist-links" />
        <small>{resolving ? "正在解析整首播放…" : track.album || track.providerId}</small>
        <button
          className="catalog-play"
          type="button"
          disabled={resolving}
          onClick={onPlay}
          aria-label={`播放 ${track.title}`}
        >
        {resolving ? <LoaderCircle size={19} className="spin" /> : <Play size={18} fill="currentColor" />}
        </button>
      </div>
      <button
        type="button"
        className="catalog-enqueue"
        onClick={onEnqueue}
        aria-label={`将 ${track.title} 添加到队列`}
        title="添加到队列（播放到时再解析）"
      >
        <ListPlus size={18} />
      </button>
    </div>
  );
}
