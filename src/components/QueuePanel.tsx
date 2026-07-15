import { useEffect, useId, useState } from "react";
import type { PlayMode } from "../types";
import "./QueuePanel.css";

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
  const headingId = useId();
  const descriptionId = useId();
  const [reorderAnnouncement, setReorderAnnouncement] = useState("");

  useEffect(() => {
    if (!open) setReorderAnnouncement("");
  }, [open]);

  if (!open) return null;
  const unavailableCount = rows.filter((row) => row.unavailable).length;

  const reorder = (from: number, to: number) => {
    if (
      from === to
      || from < 0
      || to < 0
      || from >= rows.length
      || to >= rows.length
    ) return;

    onReorder(from, to);
    setReorderAnnouncement(`《${rows[from].title}》已移至第 ${to + 1} 位，共 ${rows.length} 首。`);
  };

  return (
    <aside
      className="queue-panel"
      aria-labelledby={headingId}
      aria-describedby={descriptionId}
    >
      <header className="queue-panel-header">
        <div>
          <p className="eyebrow">QUEUE</p>
          <h3 id={headingId}>播放队列</h3>
          <small id={descriptionId}>
            {rows.length
              ? `${rows.length} 首 · ${PLAY_MODE_LABEL[playMode] ?? playMode} · 支持拖拽与键盘排序`
              : "队列为空"}
          </small>
        </div>
        <div className="queue-panel-actions">
          <button type="button" disabled={!rows.length} onClick={onClear} aria-label="清空播放队列">清空</button>
          <button type="button" onClick={onClose} aria-label="关闭播放队列">×</button>
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
        <div className="queue-empty" role="status">
          <p>还没有歌曲</p>
          <span>在曲库或搜索结果里点一首，会把当前列表整队入列。</span>
        </div>
      ) : (
        <ul className="queue-list" aria-label="队列曲目">
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
                const dragData = event.dataTransfer.getData("text/plain");
                if (!/^\d+$/.test(dragData)) return;
                const from = Number(dragData);
                if (Number.isInteger(from)) reorder(from, index);
              }}
            >
              <button
                type="button"
                className="queue-main"
                disabled={row.unavailable}
                aria-current={row.active ? "true" : undefined}
                aria-label={
                  row.unavailable
                    ? `《${row.title}》暂不可用，${row.subtitle}，第 ${index + 1} 首，共 ${rows.length} 首`
                    : `${row.active ? "当前播放" : "播放"}《${row.title}》，${row.subtitle}，第 ${index + 1} 首，共 ${rows.length} 首`
                }
                onClick={() => onJump(index)}
              >
                <span className="queue-index" aria-hidden="true">{row.unavailable ? "!" : row.active ? "♪" : String(index + 1).padStart(2, "0")}</span>
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
                    aria-label={`${row.relinking ? "正在重新定位" : "重新定位"}《${row.title}》`}
                    onClick={() => onRelink(index)}
                  >
                    {row.relinking ? "定位中…" : "重新定位"}
                  </button>
                )}
                <span className="queue-reorder-actions" role="group" aria-label={`调整《${row.title}》的位置`}>
                  <button
                    type="button"
                    className="icon-button queue-move-button"
                    disabled={index === 0}
                    aria-label={index === 0
                      ? `《${row.title}》已在队首，无法上移`
                      : `将《${row.title}》上移至第 ${index} 位`}
                    onClick={() => reorder(index, index - 1)}
                  >
                    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
                      <path d="M3.5 9.5 8 5l4.5 4.5" />
                    </svg>
                  </button>
                  <button
                    type="button"
                    className="icon-button queue-move-button"
                    disabled={index === rows.length - 1}
                    aria-label={index === rows.length - 1
                      ? `《${row.title}》已在队尾，无法下移`
                      : `将《${row.title}》下移至第 ${index + 2} 位`}
                    onClick={() => reorder(index, index + 1)}
                  >
                    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
                      <path d="m3.5 6.5 4.5 4.5 4.5-4.5" />
                    </svg>
                  </button>
                </span>
                <button type="button" className="icon-button" aria-label={`从队列移除《${row.title}》`} onClick={() => onRemove(index)}>×</button>
              </span>
            </li>
          ))}
        </ul>
      )}
      <div className="queue-sr-only" role="status" aria-live="polite" aria-atomic="true">
        {reorderAnnouncement}
      </div>
    </aside>
  );
}
