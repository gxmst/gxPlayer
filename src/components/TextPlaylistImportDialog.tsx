import { useEffect, useMemo, useState } from "react";
import type { CatalogTrack } from "../types";
import {
  createTextPlaylistSearch,
  useTextPlaylistImport,
  type TextPlaylistImportRow,
  type TextPlaylistInvoke,
  type TextPlaylistSearch,
} from "../hooks/useTextPlaylistImport";
import "./TextPlaylistImportDialog.css";

export type TextPlaylistImportDialogProps = {
  open: boolean;
  onClose: () => void;
  onEnqueue: (tracks: CatalogTrack[]) => void | Promise<void>;
  /** Inject a search function in tests or alternate frontends. */
  search?: TextPlaylistSearch;
  /** Convenience injection for the existing Tauri invoke API. */
  invoke?: TextPlaylistInvoke;
  searchLimit?: number;
  delayMs?: number;
};

const EMPTY_SEARCH: TextPlaylistSearch = async () => [];

function statusLabel(row: TextPlaylistImportRow): string {
  switch (row.status) {
    case "pending": return "等待";
    case "searching": return "正在搜索…";
    case "matched": return "已匹配";
    case "not_found": return "未找到";
    case "error": return "搜索失败";
    case "invalid": return "无法处理";
    case "cancelled": return "已取消";
  }
}

function rowTrackLabel(row: TextPlaylistImportRow): string {
  if (!row.track) return row.error ?? "";
  return `${row.track.title}${row.track.artist ? ` · ${row.track.artist}` : ""}`;
}

export function TextPlaylistImportDialog({
  open,
  onClose,
  onEnqueue,
  search,
  invoke,
  searchLimit = 5,
  delayMs = 300,
}: TextPlaylistImportDialogProps) {
  const resolvedSearch = useMemo(
    () => search ?? (invoke ? createTextPlaylistSearch(invoke, searchLimit) : EMPTY_SEARCH),
    [invoke, search, searchLimit],
  );
  const { state, start, cancel, reset } = useTextPlaylistImport(resolvedSearch, { delayMs });
  const [text, setText] = useState("");
  const [enqueueBusy, setEnqueueBusy] = useState(false);
  const [enqueueError, setEnqueueError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      cancel();
      reset();
      setText("");
      setEnqueueBusy(false);
      setEnqueueError(null);
    }
  }, [cancel, open, reset]);

  if (!open) return null;

  const matchedTracks = state.rows.flatMap((row) => row.status === "matched" && row.track ? [row.track] : []);
  const running = state.phase === "running";
  const canStart = text.trim().length > 0 && !running && !enqueueBusy;

  const close = () => {
    if (running) cancel();
    onClose();
  };

  const enqueue = async () => {
    if (!matchedTracks.length || enqueueBusy) return;
    setEnqueueBusy(true);
    setEnqueueError(null);
    try {
      await onEnqueue(matchedTracks);
      onClose();
    } catch (error) {
      setEnqueueError(String(error).slice(0, 240) || "加入队列失败");
    } finally {
      setEnqueueBusy(false);
    }
  };

  return (
    <div className="modal-backdrop text-playlist-backdrop" role="presentation" onMouseDown={(event) => {
      if (event.target === event.currentTarget) close();
    }}>
      <section
        className="text-playlist-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="text-playlist-dialog-title"
        aria-describedby="text-playlist-dialog-description"
      >
        <header className="text-playlist-dialog-header">
          <div>
            <p className="eyebrow">TEXT LIST</p>
            <h2 id="text-playlist-dialog-title">导入文本列表</h2>
            <p id="text-playlist-dialog-description">
              每行一首，支持“歌名 - 歌手”或纯歌名。这里只做搜索匹配，不会提前解析音频。
            </p>
          </div>
          <button type="button" className="icon-button" onClick={close} aria-label="关闭文本列表导入">×</button>
        </header>

        <label className="text-playlist-input-label" htmlFor="text-playlist-input">歌曲列表</label>
        <textarea
          id="text-playlist-input"
          className="text-playlist-input"
          value={text}
          onChange={(event) => {
            if (state.phase !== "idle") reset();
            setText(event.target.value);
          }}
          placeholder={'例如：\n歌曲名 - 歌手\n另一首歌'}
          maxLength={50_000}
          disabled={running || enqueueBusy}
          rows={8}
        />

        <div className="text-playlist-toolbar">
          <span>{text.length.toLocaleString()} / 50,000 字符</span>
          <button type="button" className="primary" disabled={!canStart} onClick={() => void start(text)}>
            {running ? "正在匹配…" : "开始匹配"}
          </button>
        </div>

        {state.phase !== "idle" && (
          <div className="text-playlist-progress" role="status" aria-live="polite">
            <span>{state.phase === "running" ? "正在逐行搜索" : state.phase === "cancelled" ? "匹配已取消" : "匹配完成"}</span>
            <strong>{state.processed} / {state.total}</strong>
            <span>已匹配 {state.matched} 首</span>
          </div>
        )}

        {state.warnings.length > 0 && (
          <ul className="text-playlist-warnings" role="note">
            {state.warnings.map((warning) => <li key={warning}>{warning}</li>)}
          </ul>
        )}

        {state.rows.length > 0 && (
          <div className="text-playlist-results" aria-label="文本列表匹配结果">
            {state.rows.map((row) => (
              <div className={`text-playlist-row status-${row.status}`} key={`${row.lineNumber}:${row.raw}`}>
                <span className="text-playlist-line-number">{row.lineNumber}</span>
                <span className="text-playlist-row-copy">
                  <strong title={row.raw}>{row.raw}</strong>
                  <small>{rowTrackLabel(row)}</small>
                </span>
                <span className="text-playlist-row-status">{statusLabel(row)}</span>
              </div>
            ))}
          </div>
        )}

        {enqueueError && <p className="text-playlist-error" role="alert">{enqueueError}</p>}

        <footer className="text-playlist-actions">
          <button type="button" onClick={close}>{running ? "取消" : "关闭"}</button>
          <button
            type="button"
            className="primary"
            disabled={!matchedTracks.length || running || enqueueBusy}
            onClick={() => void enqueue()}
          >
            {enqueueBusy ? "正在加入…" : `加入队列${matchedTracks.length ? `（${matchedTracks.length} 首）` : ""}`}
          </button>
        </footer>
      </section>
    </div>
  );
}
