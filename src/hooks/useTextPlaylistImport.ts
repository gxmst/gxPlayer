import { useCallback, useEffect, useRef, useState } from "react";
import type { CatalogTrack } from "../types";
import {
  DEFAULT_TEXT_PLAYLIST_MAX_LINE_LENGTH,
  DEFAULT_TEXT_PLAYLIST_MAX_LINES,
  chooseCatalogMatch,
  parseTextPlaylist,
  type ParsedTextPlaylistLine,
  type TextPlaylistParseOptions,
} from "../lib/textPlaylistImport";

export type TextPlaylistSearch = (
  query: string,
  signal: AbortSignal,
) => Promise<readonly CatalogTrack[] | null>;

export type TextPlaylistInvoke = (
  command: string,
  args?: Record<string, unknown>,
) => Promise<unknown>;

/** Adapt the existing Tauri search command without coupling this hook to Tauri. */
export function createTextPlaylistSearch(
  invoke: TextPlaylistInvoke,
  limit = 5,
): TextPlaylistSearch {
  return async (query, signal) => {
    if (signal.aborted) throw new DOMException("搜索已取消", "AbortError");
    const result = await invoke("metadata_search", { query, limit });
    return Array.isArray(result) ? result as CatalogTrack[] : null;
  };
}

export type TextPlaylistRowStatus =
  | "pending"
  | "searching"
  | "matched"
  | "not_found"
  | "error"
  | "invalid"
  | "cancelled";

export type TextPlaylistImportRow = ParsedTextPlaylistLine & {
  status: TextPlaylistRowStatus;
  track: CatalogTrack | null;
  error: string | null;
};

export type TextPlaylistImportPhase = "idle" | "running" | "complete" | "cancelled";

export type TextPlaylistImportState = {
  phase: TextPlaylistImportPhase;
  rows: TextPlaylistImportRow[];
  total: number;
  processed: number;
  matched: number;
  unresolved: number;
  warnings: string[];
};

export type TextPlaylistImportOptions = TextPlaylistParseOptions & {
  /** Minimum quiet time after one completed query before starting the next. */
  delayMs?: number;
};

export type TextPlaylistImportSummary = {
  rows: TextPlaylistImportRow[];
  matchedTracks: CatalogTrack[];
  warnings: string[];
};

const EMPTY_STATE: TextPlaylistImportState = {
  phase: "idle",
  rows: [],
  total: 0,
  processed: 0,
  matched: 0,
  unresolved: 0,
  warnings: [],
};

function invalidRow(lineNumber: number, raw: string, reason: string): TextPlaylistImportRow {
  return {
    lineNumber,
    raw,
    query: "",
    title: raw,
    artist: "",
    key: "",
    status: "invalid",
    track: null,
    error: reason,
  };
}

function initialRows(
  parsed: ReturnType<typeof parseTextPlaylist>,
): TextPlaylistImportRow[] {
  const validRows = parsed.lines.map((line) => ({
    ...line,
    status: "pending" as const,
    track: null,
    error: null,
  }));
  const invalidRows = parsed.rejected.map((line) => invalidRow(line.lineNumber, line.raw, line.reason));
  return [...validRows, ...invalidRows].sort((left, right) => left.lineNumber - right.lineNumber);
}

function summarizeRows(rows: readonly TextPlaylistImportRow[]) {
  const terminal = new Set<TextPlaylistRowStatus>([
    "matched",
    "not_found",
    "error",
    "invalid",
    "cancelled",
  ]);
  const processed = rows.filter((row) => terminal.has(row.status)).length;
  const matched = rows.filter((row) => row.status === "matched" && row.track).length;
  return { processed, matched, unresolved: processed - matched };
}

function errorText(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  const trimmed = message.trim();
  return (trimmed || "搜索失败").slice(0, 240);
}

function waitFor(ms: number, signal: AbortSignal): Promise<boolean> {
  if (ms <= 0) return Promise.resolve(!signal.aborted);
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      resolve(value);
    };
    const onAbort = () => finish(false);
    const timer = window.setTimeout(() => finish(true), ms);
    if (signal.aborted) finish(false);
    else signal.addEventListener("abort", onAbort, { once: true });
  });
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/**
 * Runs user-provided text searches sequentially. One logical search already
 * fans out to the existing metadata providers, so parallel lines would multiply
 * outbound traffic unnecessarily.
 */
export function useTextPlaylistImport(
  search: TextPlaylistSearch,
  options: TextPlaylistImportOptions = {},
) {
  const [state, setState] = useState<TextPlaylistImportState>(EMPTY_STATE);
  const generationRef = useRef(0);
  const controllerRef = useRef<AbortController | null>(null);
  const searchRef = useRef(search);
  const rowsRef = useRef<TextPlaylistImportRow[]>([]);
  searchRef.current = search;

  const delayMs = Math.max(0, Math.floor(options.delayMs ?? 300));
  const maxLines = options.maxLines ?? DEFAULT_TEXT_PLAYLIST_MAX_LINES;
  const maxLineLength = options.maxLineLength ?? DEFAULT_TEXT_PLAYLIST_MAX_LINE_LENGTH;

  const cancel = useCallback(() => {
    generationRef.current += 1;
    controllerRef.current?.abort();
    controllerRef.current = null;
    const cancelledRows = rowsRef.current.map((row) => (
      row.status === "pending" || row.status === "searching"
        ? { ...row, status: "cancelled" as const, error: "已取消" }
        : row
    ));
    rowsRef.current = cancelledRows;
    setState((previous) => {
      if (previous.phase !== "running") return previous;
      const summary = summarizeRows(cancelledRows);
      return { ...previous, phase: "cancelled", rows: cancelledRows, ...summary };
    });
  }, []);

  const reset = useCallback(() => {
    generationRef.current += 1;
    controllerRef.current?.abort();
    controllerRef.current = null;
    rowsRef.current = [];
    setState(EMPTY_STATE);
  }, []);

  const start = useCallback(async (text: string): Promise<TextPlaylistImportSummary | null> => {
    controllerRef.current?.abort();
    const generation = ++generationRef.current;
    const controller = new AbortController();
    controllerRef.current = controller;
    const parsed = parseTextPlaylist(text, { maxLines, maxLineLength });
    let workingRows = initialRows(parsed);
    rowsRef.current = workingRows;
    setState({
      phase: "running",
      rows: workingRows,
      total: workingRows.length,
      ...summarizeRows(workingRows),
      warnings: parsed.warnings,
    });

    const active = () => generation === generationRef.current && !controller.signal.aborted;
    const updateRow = (lineNumber: number, patch: Partial<TextPlaylistImportRow>) => {
      if (!active()) return;
      workingRows = workingRows.map((row) => row.lineNumber === lineNumber ? { ...row, ...patch } : row);
      rowsRef.current = workingRows;
      const summary = summarizeRows(workingRows);
      setState((previous) => generation === generationRef.current
        ? { ...previous, rows: workingRows, ...summary }
        : previous);
    };

    const cache = new Map<string, Promise<readonly CatalogTrack[] | null>>();
    let hasSearched = false;
    try {
      for (const line of parsed.lines) {
        if (!active()) return null;
        if (hasSearched && !cache.has(line.key) && !(await waitFor(delayMs, controller.signal))) return null;
        if (!active()) return null;
        hasSearched = true;
        updateRow(line.lineNumber, { status: "searching", error: null });

        try {
          let request = cache.get(line.key);
          if (!request) {
            request = Promise.resolve(searchRef.current(line.query, controller.signal));
            cache.set(line.key, request);
          }
          const candidates = await request;
          if (!active()) return null;
          const track = chooseCatalogMatch(line, candidates ?? []);
          if (track) updateRow(line.lineNumber, { status: "matched", track, error: null });
          else updateRow(line.lineNumber, { status: "not_found", track: null, error: "未找到匹配歌曲" });
        } catch (error) {
          if (!active() || isAbortError(error)) return null;
          updateRow(line.lineNumber, { status: "error", track: null, error: errorText(error) });
        }
      }

      if (!active()) return null;
      const summary = summarizeRows(workingRows);
      const result: TextPlaylistImportSummary = {
        rows: workingRows,
        matchedTracks: workingRows.flatMap((row) => row.status === "matched" && row.track ? [row.track] : []),
        warnings: parsed.warnings,
      };
      setState((previous) => generation === generationRef.current
        ? { ...previous, phase: "complete", rows: workingRows, ...summary }
        : previous);
      return result;
    } finally {
      if (generation === generationRef.current) controllerRef.current = null;
    }
  }, [delayMs, maxLineLength, maxLines]);

  useEffect(() => () => {
    generationRef.current += 1;
    controllerRef.current?.abort();
  }, []);

  return { state, start, cancel, reset };
}
