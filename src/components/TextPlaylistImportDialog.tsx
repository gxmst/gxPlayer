import { useEffect, useMemo, useRef, useState } from "react";
import type { CatalogTrack } from "../types";
import { TEXT_PLAYLIST_CONFIDENCE_THRESHOLD } from "../lib/textPlaylistImport";
import {
  buildTextPlaylistUnmatchedText,
  collectIncludedTextPlaylistTracks,
  createTextPlaylistSearch,
  useTextPlaylistImport,
  type TextPlaylistImportRow,
  type TextPlaylistInvoke,
  type TextPlaylistSearch,
} from "../hooks/useTextPlaylistImport";
import { Dialog } from "./Dialog";
import "./TextPlaylistImportDialog.css";

export type TextPlaylistImportDialogProps = {
  open: boolean;
  onClose: () => void;
  onEnqueue: (tracks: CatalogTrack[]) => void | Promise<void>;
  onExportUnmatched?: (text: string) => void | Promise<void>;
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
    case "matched": {
      if (row.included) return (row.confidence ?? 0) < TEXT_PLAYLIST_CONFIDENCE_THRESHOLD ? "已确认" : "已匹配";
      return (row.confidence ?? 0) < TEXT_PLAYLIST_CONFIDENCE_THRESHOLD ? "待确认" : "不加入";
    }
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

function candidateLabel(row: TextPlaylistImportRow, index: number): string {
  const candidate = row.candidates[index];
  if (!candidate) return "";
  const { track, score } = candidate;
  const details = [track.artist, track.album].filter(Boolean).join(" · ");
  return `${track.title}${details ? ` · ${details}` : ""}（${Math.round(score * 100)}%）`;
}

export function TextPlaylistImportDialog({
  open,
  onClose,
  onEnqueue,
  onExportUnmatched,
  search,
  invoke,
  searchLimit = 5,
  delayMs = 300,
}: TextPlaylistImportDialogProps) {
  const resolvedSearch = useMemo(
    () => search ?? (invoke ? createTextPlaylistSearch(invoke, searchLimit) : EMPTY_SEARCH),
    [invoke, search, searchLimit],
  );
  const { state, start, cancel, reset, setRowIncluded, selectCandidate } = useTextPlaylistImport(resolvedSearch, { delayMs });
  const [text, setText] = useState("");
  const [enqueueBusy, setEnqueueBusy] = useState(false);
  const [enqueueError, setEnqueueError] = useState<string | null>(null);
  const [exportBusy, setExportBusy] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const initialFocusRef = useRef<HTMLTextAreaElement>(null);
  const enqueueLockRef = useRef(false);
  const exportLockRef = useRef(false);

  useEffect(() => {
    if (!open) {
      cancel();
      reset();
      setText("");
      setEnqueueBusy(false);
      setEnqueueError(null);
      setExportBusy(false);
      setExportError(null);
      enqueueLockRef.current = false;
      exportLockRef.current = false;
    }
  }, [cancel, open, reset]);

  if (!open) return null;

  const matchedTracks = collectIncludedTextPlaylistTracks(state.rows);
  const unmatchedText = buildTextPlaylistUnmatchedText(state.rows);
  const running = state.phase === "running";
  const submissionBusy = enqueueBusy || exportBusy;
  const canStart = text.trim().length > 0 && !running && !submissionBusy;

  const close = () => {
    if (enqueueLockRef.current || exportLockRef.current || submissionBusy) return;
    if (running) cancel();
    onClose();
  };

  const enqueue = async () => {
    if (
      !matchedTracks.length
      || enqueueLockRef.current
      || exportLockRef.current
      || submissionBusy
    ) return;
    enqueueLockRef.current = true;
    setEnqueueBusy(true);
    setEnqueueError(null);
    setExportError(null);
    try {
      await onEnqueue(matchedTracks);
      onClose();
    } catch (error) {
      setEnqueueError(String(error).slice(0, 240) || "加入队列失败");
    } finally {
      enqueueLockRef.current = false;
      setEnqueueBusy(false);
    }
  };

  const exportUnmatched = async () => {
    if (
      !onExportUnmatched
      || !unmatchedText
      || exportLockRef.current
      || enqueueLockRef.current
      || submissionBusy
    ) return;
    exportLockRef.current = true;
    setExportBusy(true);
    setEnqueueError(null);
    setExportError(null);
    try {
      await onExportUnmatched(unmatchedText);
    } catch (error) {
      setExportError(String(error).slice(0, 240) || "导出失败");
    } finally {
      exportLockRef.current = false;
      setExportBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      title="导入文本列表"
      eyebrow="TEXT LIST"
      description="每行一首，支持“歌名 - 歌手”或纯歌名。这里只做搜索匹配，不会提前解析音频。"
      actions={(
        <>
          <button type="button" disabled={submissionBusy} onClick={close}>{running ? "取消" : "关闭"}</button>
          {onExportUnmatched && unmatchedText && !running && (
            <button type="button" disabled={submissionBusy} onClick={() => void exportUnmatched()}>
              {exportBusy ? "正在导出…" : `导出未匹配（${state.unmatched} 行）`}
            </button>
          )}
          <button
            type="button"
            className="primary"
            disabled={!matchedTracks.length || running || submissionBusy}
            onClick={() => void enqueue()}
          >
            {enqueueBusy ? "正在加入…" : `确认加入队列${matchedTracks.length ? `（${matchedTracks.length} 首）` : ""}`}
          </button>
        </>
      )}
      size="large"
      className="text-playlist-dialog"
      busy={submissionBusy}
      showClose
      closeOnBackdrop
      initialFocusRef={initialFocusRef}
      onRequestClose={close}
    >
      <label className="text-playlist-input-label" htmlFor="text-playlist-input">歌曲列表</label>
      <textarea
        ref={initialFocusRef}
        id="text-playlist-input"
        className="text-playlist-input"
        value={text}
        onChange={(event) => {
          if (state.phase !== "idle") reset();
          setText(event.target.value);
        }}
        placeholder={'例如：\n歌曲名 - 歌手\n另一首歌'}
        maxLength={50_000}
        disabled={running || submissionBusy}
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

        {(state.phase === "complete" || state.phase === "cancelled") && (
          <div className="text-playlist-summary" aria-label="入队摘要">
            <strong>入队摘要</strong>
            <span>准备加入 {state.included} 首</span>
            <span>待确认 {state.needsConfirmation} 首</span>
            <span>未匹配 {state.unresolved} 首</span>
            <span>已取消选择 {state.excluded} 首</span>
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
                <span className="text-playlist-row-check">
                  {row.status === "matched" && row.track && (
                    <input
                      type="checkbox"
                      aria-label={`第 ${row.lineNumber} 行加入队列`}
                      checked={row.included}
                      disabled={running || submissionBusy}
                      onChange={(event) => setRowIncluded(row.lineNumber, event.target.checked)}
                    />
                  )}
                </span>
                <span className="text-playlist-row-copy">
                  <strong title={row.raw}>{row.raw}</strong>
                  <small>
                    {row.status === "matched" && row.confidence !== null
                      ? `匹配度 ${Math.round(row.confidence * 100)}%`
                      : rowTrackLabel(row)}
                  </small>
                </span>
                <span className="text-playlist-row-candidate">
                  {row.status === "matched" && row.track && row.candidates.length > 1 ? (
                    <select
                      aria-label={`第 ${row.lineNumber} 行候选版本`}
                      value={row.selectedCandidateIndex ?? 0}
                      disabled={running || submissionBusy}
                      onChange={(event) => selectCandidate(row.lineNumber, Number(event.target.value))}
                    >
                      {row.candidates.map((candidate, index) => (
                        <option
                          key={`${candidate.track.providerId}:${candidate.track.providerTrackId}:${candidate.sourceIndex}`}
                          value={index}
                        >
                          {candidateLabel(row, index)}
                        </option>
                      ))}
                    </select>
                  ) : row.status === "matched" && row.track ? (
                    <small title={rowTrackLabel(row)}>{rowTrackLabel(row)}</small>
                  ) : null}
                </span>
                <span className="text-playlist-row-status">{statusLabel(row)}</span>
              </div>
            ))}
          </div>
        )}

      {(enqueueError || exportError) && <p className="text-playlist-error" role="alert">{enqueueError ?? exportError}</p>}
    </Dialog>
  );
}
