import type { PlayMode } from "../types";

export type QueueRow = {
  key: string;
  title: string;
  subtitle: string;
  active: boolean;
  unavailable: boolean;
  relinking?: boolean;
};

export type QueueAvailabilityStatus = "checking" | "ready" | "failed";

const PLAY_MODE_LABEL: Record<PlayMode, string> = {
  sequential: "顺序播放",
  repeat_all: "列表循环",
  repeat_one: "单曲循环",
  shuffle: "随机播放",
};

type Props = {
  open: boolean;
  rows: QueueRow[];
  playMode: PlayMode;
  availabilityStatus: QueueAvailabilityStatus;
  onClose: () => void;
  onClear: () => void;
  onJump: (index: number) => void;
  onRelink: (index: number) => void;
  onRetryAvailability: () => void;
  onRemove: (index: number) => void;
  onReorder: (from: number, to: number) => void;
};

export function QueuePanel({
  open,
  rows,
  playMode,
  availabilityStatus,
  onClose,
  onClear,
  onJump,
  onRelink,
  onRetryAvailability,
  onRemove,
  onReorder,
}: Props) {
  if (!open) return null;
  const unavailableCount = rows.filter((row) => row.unavailable).length;

  return (
    <aside className="queue-panel" aria-label="播放队列">
      <header className="queue-panel-header">
        <div>
          <p className="eyebrow">QUEUE</p>
          <h3>播放队列</h3>
          <small>
            {rows.length
              ? `${rows.length} 首 · ${PLAY_MODE_LABEL[playMode] ?? playMode} · 可拖拽排序`
              : "队列为空"}
          </small>
        </div>
        <div className="queue-panel-actions">
          <button type="button" disabled={!rows.length} onClick={onClear}>清空</button>
          <button type="button" onClick={onClose} aria-label="关闭队列">×</button>
        </div>
      </header>
      {(availabilityStatus !== "ready" || unavailableCount > 0) && (
        <div className={`queue-availability ${availabilityStatus === "failed" ? "failed" : ""}`} role="status" aria-live="polite">
          <span>
            {availabilityStatus === "checking"
              ? "正在检查本地文件…"
              : availabilityStatus === "failed"
                ? "本地文件检查失败，队列仍已完整保留。"
                : `${unavailableCount} 首本地歌曲暂不可用，接回磁盘后可重试。`}
          </span>
          {availabilityStatus !== "checking" && (
            <button type="button" onClick={onRetryAvailability}>重试检查</button>
          )}
        </div>
      )}
      {rows.length === 0 ? (
        <div className="queue-empty">
          <p>还没有歌曲</p>
          <span>在曲库或搜索结果里点一首，会把当前列表整队入列。</span>
        </div>
      ) : (
        <ul className="queue-list">
          {rows.map((row, index) => (
            <li
              key={row.key}
              className={`${row.active ? "active" : ""} ${row.unavailable ? "unavailable" : ""}`.trim()}
              draggable
              onDragStart={(event) => {
                event.dataTransfer.setData("text/plain", String(index));
                event.dataTransfer.effectAllowed = "move";
              }}
              onDragOver={(event) => {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
              }}
              onDrop={(event) => {
                event.preventDefault();
                const from = Number(event.dataTransfer.getData("text/plain"));
                if (Number.isFinite(from)) onReorder(from, index);
              }}
            >
              <button type="button" className="queue-main" disabled={row.unavailable} onClick={() => onJump(index)}>
                <span className="queue-index">{row.unavailable ? "!" : row.active ? "♪" : String(index + 1).padStart(2, "0")}</span>
                <span>
                  <strong>{row.title}</strong>
                  <small>{row.subtitle}</small>
                </span>
              </button>
              <span className="queue-row-actions">
                {row.unavailable && (
                  <button
                    type="button"
                    className="queue-relink"
                    disabled={row.relinking}
                    onClick={() => onRelink(index)}
                  >
                    {row.relinking ? "定位中…" : "重新定位"}
                  </button>
                )}
                <button type="button" className="icon-button" aria-label={`从队列移除《${row.title}》`} onClick={() => onRemove(index)}>×</button>
              </span>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
