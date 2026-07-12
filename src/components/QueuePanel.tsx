import type { PlayMode } from "../types";

export type QueueRow = {
  key: string;
  title: string;
  subtitle: string;
  active: boolean;
};

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
  onClose: () => void;
  onClear: () => void;
  onJump: (index: number) => void;
  onRemove: (index: number) => void;
  onReorder: (from: number, to: number) => void;
};

export function QueuePanel({
  open,
  rows,
  playMode,
  onClose,
  onClear,
  onJump,
  onRemove,
  onReorder,
}: Props) {
  if (!open) return null;

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
              className={row.active ? "active" : ""}
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
              <button type="button" className="queue-main" onClick={() => onJump(index)}>
                <span className="queue-index">{row.active ? "♪" : String(index + 1).padStart(2, "0")}</span>
                <span>
                  <strong>{row.title}</strong>
                  <small>{row.subtitle}</small>
                </span>
              </button>
              <button type="button" className="icon-button" aria-label="从队列移除" onClick={() => onRemove(index)}>×</button>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
