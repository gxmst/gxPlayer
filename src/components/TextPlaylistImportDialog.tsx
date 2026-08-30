import { useEffect, useMemo, useRef, useState } from "react";
import type { CatalogTrack } from "../types";
import { TEXT_PLAYLIST_CONFIDENCE_THRESHOLD } from "../lib/textPlaylistImport";
import {
  normalizePlaylistText,
  playlistFormatLabel,
  PLAYLIST_DETECTION_CONFIDENCE_THRESHOLD,
  toImportableText,
  toLocalPaths,
  type NormalizedPlaylist,
} from "../lib/playlistFormats";
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
  /** Let the user pick a playlist file; resolves to its text, or null if cancelled. */
  onOpenFile?: () => Promise<{ name: string; text: string } | null>;
  /** Import local files an m3u referenced, reading their real tags. Returns how many landed. */
  onImportLocalPaths?: (paths: string[]) => Promise<number>;
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
  onOpenFile,
  onImportLocalPaths,
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
  const [fileBusy, setFileBusy] = useState(false);
  const [fileName, setFileName] = useState<string | null>(null);
  const [localImportNote, setLocalImportNote] = useState<string | null>(null);
  const initialFocusRef = useRef<HTMLTextAreaElement>(null);
  const enqueueLockRef = useRef(false);
  const exportLockRef = useRef(false);
  const fileLockRef = useRef(false);

  useEffect(() => {
    if (!open) {
      cancel();
      reset();
      setText("");
      setEnqueueBusy(false);
      setEnqueueError(null);
      setExportBusy(false);
      setExportError(null);
      setFileBusy(false);
      setFileName(null);
      setLocalImportNote(null);
      enqueueLockRef.current = false;
      exportLockRef.current = false;
      fileLockRef.current = false;
    }
  }, [cancel, open, reset]);

  // Detection is pure and cheap, so it can follow the textarea directly.
  const normalized: NormalizedPlaylist | null = useMemo(
    () => (text.trim() ? normalizePlaylistText(text) : null),
    [text],
  );

  if (!open) return null;

  const matchedTracks = collectIncludedTextPlaylistTracks(state.rows);
  const unmatchedText = buildTextPlaylistUnmatchedText(state.rows);
  const running = state.phase === "running";
  const submissionBusy = enqueueBusy || exportBusy || fileBusy;
  const localPaths = normalized ? toLocalPaths(normalized.entries) : [];
  const searchableText = normalized ? toImportableText(normalized.entries) : "";
  const canStart = Boolean(normalized?.entries.length) && !running && !submissionBusy;

  const close = () => {
    if (
      enqueueLockRef.current
      || exportLockRef.current
      || fileLockRef.current
      || submissionBusy
    ) return;
    if (running) cancel();
    onClose();
  };

  const replaceText = (next: string, name: string | null) => {
    if (state.phase !== "idle") reset();
    setLocalImportNote(null);
    setEnqueueError(null);
    setExportError(null);
    setFileName(name);
    setText(next);
  };

  const openFile = async () => {
    if (!onOpenFile || fileLockRef.current || submissionBusy || running) return;
    fileLockRef.current = true;
    setFileBusy(true);
    setExportError(null);
    try {
      const picked = await onOpenFile();
      if (picked) replaceText(picked.text, picked.name);
    } catch (error) {
      setExportError(String(error).slice(0, 240) || "读取文件失败");
    } finally {
      fileLockRef.current = false;
      setFileBusy(false);
    }
  };

  /**
   * Local files are imported through the library so their own tags are used.
   * Only entries without a local path go on to the online search.
   */
  const startMatching = async () => {
    if (!canStart || !normalized) return;
    if (localPaths.length && onImportLocalPaths) {
      fileLockRef.current = true;
      setFileBusy(true);
      setLocalImportNote(null);
      try {
        const imported = await onImportLocalPaths(localPaths);
        setLocalImportNote(`已从本地文件导入 ${imported} 首到曲库。`);
      } catch (error) {
        setLocalImportNote(`本地文件导入失败：${String(error).slice(0, 160)}`);
      } finally {
        fileLockRef.current = false;
        setFileBusy(false);
      }
    }
    if (searchableText.trim()) void start(searchableText);
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
      title="导入歌单"
      eyebrow="PLAYLIST"
      description="可以粘贴文本，也可以打开 m3u/m3u8 或 CSV 文件。格式会自动识别；这里只做搜索匹配，不会提前解析音频。"
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
      <div className="text-playlist-input-header">
        <label className="text-playlist-input-label" htmlFor="text-playlist-input">歌曲列表</label>
        {onOpenFile && (
          <button type="button" disabled={running || submissionBusy} onClick={() => void openFile()}>
            {fileBusy ? "正在读取…" : "打开歌单文件…"}
          </button>
        )}
      </div>
      <textarea
        ref={initialFocusRef}
        id="text-playlist-input"
        className="text-playlist-input"
        value={text}
        onChange={(event) => replaceText(event.target.value, null)}
        placeholder={'例如：\n歌曲名 - 歌手\n另一首歌\n\n也可以直接粘贴 m3u 或 CSV 内容'}
        maxLength={50_000}
        disabled={running || submissionBusy}
        rows={8}
      />

        {normalized && normalized.entries.length > 0 && (
          <div className="text-playlist-detection" role="status" aria-live="polite">
            <strong>{fileName ? `${fileName}：` : "已识别："}</strong>
            <span>
              {normalized.confidence >= PLAYLIST_DETECTION_CONFIDENCE_THRESHOLD
                ? playlistFormatLabel(normalized.format)
                : `可能是${playlistFormatLabel(normalized.format)}`}
              {` · ${normalized.entries.length} 条`}
            </span>
            {localPaths.length > 0 && <span>{localPaths.length} 条本地文件</span>}
            {searchableText.trim() && localPaths.length > 0 && (
              <span>{normalized.entries.length - localPaths.length} 条需联网匹配</span>
            )}
            {normalized.skipped.length > 0 && <span>已跳过 {normalized.skipped.length} 条</span>}
          </div>
        )}

        {normalized && normalized.notes.length > 0 && (
          <ul className="text-playlist-notes">
            {normalized.notes.map((note) => <li key={note}>{note}</li>)}
          </ul>
        )}

        {localImportNote && <p className="text-playlist-local-note" role="status">{localImportNote}</p>}

        <div className="text-playlist-toolbar">
          <span>{text.length.toLocaleString()} / 50,000 字符</span>
          <button type="button" className="primary" disabled={!canStart} onClick={() => void startMatching()}>
            {running ? "正在匹配…" : fileBusy ? "正在导入本地文件…" : "开始匹配"}
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
