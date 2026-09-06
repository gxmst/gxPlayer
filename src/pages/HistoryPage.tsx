import type { CatalogTrack, HistoryEntry, LibraryTrack } from "../types";
import { groupConsecutiveHistory } from "../lib/historyGrouping";
import { PageHeading, EmptyState } from "../components/PageChrome";

type HistoryPageProps = {
  historyEntries: HistoryEntry[];
  onClearHistory: () => void;
  onPlayLocal: (tracks: LibraryTrack[], track: LibraryTrack) => void;
  onPlayCatalog: (track: CatalogTrack) => void;
};


export function HistoryPage({ historyEntries, onClearHistory, onPlayLocal, onPlayCatalog }: HistoryPageProps) {
  const groupedHistoryEntries = groupConsecutiveHistory(historyEntries);

  return (
    <div className="page">
      <PageHeading
        eyebrow="HISTORY"
        title="播放历史"
        copy={`${historyEntries.length} 条原始播放记录，连续同曲合并显示为 ${groupedHistoryEntries.length} 行（读取最近 500 条）。`}
        action={
          <button type="button" className="danger" disabled={!historyEntries.length} onClick={onClearHistory}>
            清空历史
          </button>
        }
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
                onClick={() => {
                  if (entry.kind === "local" && entry.path) {
                    onPlayLocal(
                      [{
                        id: -1,
                        path: entry.path,
                        title: entry.title,
                        artist: entry.artist,
                        album: "",
                        durationSeconds: null,
                        favorite: false,
                        addedAtMs: 0,
                      }],
                      {
                        id: -1,
                        path: entry.path,
                        title: entry.title,
                        artist: entry.artist,
                        album: "",
                        durationSeconds: null,
                        favorite: false,
                        addedAtMs: 0,
                      },
                    );
                  } else if (entry.providerId && entry.providerTrackId) {
                    onPlayCatalog({
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
                <span className="track-index" aria-hidden="true">
                  {entry.kind === "local" ? "♪" : entry.kind === "cached" ? "◉" : "☁"}
                </span>
                <span className="sr-only">
                  {entry.kind === "local" ? "本地播放" : entry.kind === "cached" ? "缓存播放" : "在线播放"}
                </span>
                <span>
                  <strong>{entry.title}</strong>
                  <small>
                    {entry.artist || "未知歌手"} · {new Date(entry.playedAtMs).toLocaleString()}
                  </small>
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
