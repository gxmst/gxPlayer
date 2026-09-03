import { useState, type ReactNode } from "react";
import type { LibraryTrack } from "../types";

type VirtualTrackListProps = {
  tracks: LibraryTrack[];
  renderRow: (track: LibraryTrack, index: number) => ReactNode;
};

export function VirtualTrackList({ tracks, renderRow }: VirtualTrackListProps) {
  const rowHeight = 68;
  const viewportHeight = 544;
  const [scrollTop, setScrollTop] = useState(0);
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - 4);
  const visibleCount = Math.ceil(viewportHeight / rowHeight) + 8;
  const end = Math.min(tracks.length, start + visibleCount);
  const visible = tracks.slice(start, end);
  const offsetTop = start * rowHeight;
  const totalHeight = tracks.length * rowHeight;

  return (
    <div
      className="track-list virtual-track-list"
      role="list"
      style={{ height: `${viewportHeight}px`, overflowY: "auto" }}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <div style={{ height: `${totalHeight}px`, position: "relative" }}>
        <div style={{ transform: `translateY(${offsetTop}px)` }}>
          {visible.map((track, index) => (
            <div key={track.id}>{renderRow(track, start + index)}</div>
          ))}
        </div>
      </div>
    </div>
  );
}
