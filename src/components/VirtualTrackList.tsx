import { useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { LibraryTrack } from "../types";

type VirtualTrackListProps = {
  tracks: LibraryTrack[];
  renderRow: (track: LibraryTrack, index: number) => ReactNode;
  className?: string;
};

export function VirtualTrackList({ tracks, renderRow, className = "" }: VirtualTrackListProps) {
  const rowHeight = 68;
  const viewportHeight = 544;
  const [scrollTop, setScrollTop] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const trackOrder = useMemo(() => tracks.map((track) => track.id).join(","), [tracks]);
  useLayoutEffect(() => {
    if (containerRef.current) containerRef.current.scrollTop = 0;
    setScrollTop(0);
  }, [trackOrder]);
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - 4);
  const visibleCount = Math.ceil(viewportHeight / rowHeight) + 8;
  const end = Math.min(tracks.length, start + visibleCount);
  const visible = tracks.slice(start, end);
  const offsetTop = start * rowHeight;
  const totalHeight = tracks.length * rowHeight;

  return (
    <div
      ref={containerRef}
      className={`track-list virtual-track-list ${className}`.trim()}
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
