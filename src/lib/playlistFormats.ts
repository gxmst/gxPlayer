/**
 * Recognise the common interchange formats other players write, and reduce them
 * to the plain "title - artist" text the existing importer already understands.
 *
 * Only self-contained text formats are handled: a file the user exported, or
 * text they pasted. No platform playlist link or interface is involved, and no
 * network request happens here.
 */

export type PlaylistTextFormat = "m3u" | "csv" | "plain";

export type NormalizedPlaylistEntry = {
  /** Line number in the original input, for feedback the user can act on. */
  lineNumber: number;
  /** Text handed to the text importer when there is no usable local path. */
  text: string;
  /**
   * Local file reference from an m3u, if the entry had one. Absolute paths are
   * kept verbatim; the caller decides whether the file still exists.
   */
  localPath: string | null;
};

export type SkippedPlaylistLine = {
  lineNumber: number;
  raw: string;
  reason: string;
};

export type NormalizedPlaylist = {
  format: PlaylistTextFormat;
  /** 0..1. Below DETECTION_CONFIDENCE_THRESHOLD the caller should say so. */
  confidence: number;
  entries: NormalizedPlaylistEntry[];
  skipped: SkippedPlaylistLine[];
  notes: string[];
};

export const PLAYLIST_DETECTION_CONFIDENCE_THRESHOLD = 0.72;

/** Bound the work regardless of what was pasted or opened. */
export const MAX_PLAYLIST_INPUT_LINES = 5_000;

const FORMAT_LABELS: Record<PlaylistTextFormat, string> = {
  m3u: "M3U / M3U8 播放列表",
  csv: "CSV / TSV 表格",
  plain: "纯文本列表",
};

export function playlistFormatLabel(format: PlaylistTextFormat): string {
  return FORMAT_LABELS[format];
}

function splitLines(text: string): string[] {
  return text.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n").split("\n");
}

function isRemoteUrl(value: string): boolean {
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(value);
}

/** Any path-shaped entry, used only to tell an m3u apart from a plain list. */
function looksLikePathEntry(value: string): boolean {
  if (/^file:\/\//i.test(value)) return true;
  if (isRemoteUrl(value)) return false;
  if (/^[a-z]:[\\/]/i.test(value)) return true;
  if (/^\\\\/.test(value)) return true;
  if (value.startsWith("/")) return true;
  return /[\\/]/.test(value) && /\.[a-z0-9]{2,5}$/i.test(value);
}

/** A UNC path, including the file:// form that decodes to one. */
function isUncPath(value: string): boolean {
  if (/^\\\\/.test(value)) return true;
  if (!/^file:\/\//i.test(value)) return false;
  try {
    return new URL(value).hostname.length > 0;
  } catch {
    return false;
  }
}

export type LocalPathVerdict =
  | { kind: "import"; path: string }
  | { kind: "skip"; reason: string }
  | { kind: "not_a_path" };

/**
 * Decide whether an entry may be handed to the library for tag reading.
 *
 * Only unambiguous local files qualify. Two categories are deliberately refused
 * even though they look like paths:
 *
 * - UNC and file://host paths, because opening one makes an outbound SMB
 *   connection to a host the playlist chose, which on Windows also offers up
 *   credentials. A playlist is untrusted input.
 * - Relative paths, because m3u defines them against the playlist file's own
 *   directory and we do not resolve them that way; importing one would read a
 *   file relative to whatever the process CWD happens to be.
 *
 * A refused entry is not lost: its text still goes to the online matcher.
 */
export function classifyLocalPath(value: string): LocalPathVerdict {
  if (!looksLikePathEntry(value)) return { kind: "not_a_path" };
  if (isUncPath(value)) return { kind: "skip", reason: "跳过网络共享路径" };

  const decoded = decodeFileUrl(value);
  const absolute = /^[a-z]:[\\/]/i.test(decoded) || decoded.startsWith("/") || decoded.startsWith("\\");
  if (!absolute) return { kind: "skip", reason: "跳过相对路径，将按歌名联网匹配" };
  return { kind: "import", path: decoded };
}

function decodeFileUrl(value: string): string {
  if (!/^file:\/\//i.test(value)) return value;
  try {
    const url = new URL(value);
    const pathname = decodeURIComponent(url.pathname);
    // file:///C:/x -> C:/x ; file://server/share -> \\server\share
    if (url.hostname) return `\\\\${url.hostname}${pathname.replace(/\//g, "\\")}`;
    return /^\/[a-z]:/i.test(pathname) ? pathname.slice(1) : pathname;
  } catch {
    return value;
  }
}

function detectFormat(lines: readonly string[]): { format: PlaylistTextFormat; confidence: number } {
  const meaningful = lines.map((line) => line.trim()).filter(Boolean);
  if (!meaningful.length) return { format: "plain", confidence: 0 };

  if (meaningful[0] === "#EXTM3U") return { format: "m3u", confidence: 1 };
  const extinfCount = meaningful.filter((line) => line.startsWith("#EXTINF")).length;
  if (extinfCount > 0) {
    // Directive present but no header: still unambiguously m3u.
    return { format: "m3u", confidence: 0.92 };
  }

  const pathLike = meaningful.filter((line) => !line.startsWith("#") && looksLikePathEntry(line)).length;
  if (pathLike >= Math.max(2, Math.ceil(meaningful.length * 0.6))) {
    // A bare path list is what many players export when metadata is omitted.
    return { format: "m3u", confidence: 0.78 };
  }

  const csv = detectDelimiter(meaningful);
  if (csv) return { format: "csv", confidence: csv.confidence };

  return { format: "plain", confidence: 0.8 };
}

/**
 * A delimiter only counts when it appears consistently. A single stray comma in
 * a song title must not turn the whole list into a table.
 */
function detectDelimiter(lines: readonly string[]): { delimiter: string; confidence: number } | null {
  const sample = lines.slice(0, 40);
  for (const delimiter of ["\t", ",", ";", "|"]) {
    const counts = sample.map((line) => splitDelimited(line, delimiter).length);
    const columns = counts[0];
    if (!columns || columns < 2) continue;
    const consistent = counts.filter((count) => count === columns).length;
    const ratio = consistent / counts.length;
    if (ratio < 0.8) continue;
    // Tabs are unambiguous; punctuation shared with titles earns less trust.
    const confidence = delimiter === "\t" ? 0.95 : Math.min(0.9, 0.6 + ratio * 0.3);
    return { delimiter, confidence };
  }
  return null;
}

/** Minimal RFC 4180 field splitting: quotes group, doubled quotes escape. */
function splitDelimited(line: string, delimiter: string): string[] {
  const fields: string[] = [];
  let current = "";
  let quoted = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (quoted) {
      if (char === '"') {
        if (line[index + 1] === '"') {
          current += '"';
          index += 1;
        } else quoted = false;
      } else current += char;
      continue;
    }
    if (char === '"' && !current) quoted = true;
    else if (char === delimiter) {
      fields.push(current);
      current = "";
    } else current += char;
  }
  fields.push(current);
  return fields.map((field) => field.trim());
}

const TITLE_HEADERS = ["title", "track", "name", "song", "歌曲", "歌名", "曲名", "标题"];
const ARTIST_HEADERS = ["artist", "artists", "performer", "singer", "歌手", "艺人", "演唱者"];

function headerIndex(fields: readonly string[], names: readonly string[]): number {
  return fields.findIndex((field) => names.includes(field.trim().toLowerCase()));
}

function normalizeM3u(lines: readonly string[]): NormalizedPlaylist {
  const entries: NormalizedPlaylistEntry[] = [];
  const skipped: SkippedPlaylistLine[] = [];
  const notes: string[] = [];
  let pendingTitle: string | null = null;
  let remoteEntries = 0;
  /** Path-shaped entries refused for auto-import but still matched by text. */
  const refusedPaths: SkippedPlaylistLine[] = [];

  lines.forEach((rawLine, index) => {
    const lineNumber = index + 1;
    const value = (index === 0 ? rawLine.replace(/^\uFEFF/, "") : rawLine).trim();
    if (!value) return;

    if (value.startsWith("#")) {
      const extinf = /^#EXTINF\s*:\s*(-?\d+(?:\.\d+)?)?\s*(?:,\s*(.*))?$/i.exec(value);
      if (extinf) {
        const label = (extinf[2] ?? "").trim();
        pendingTitle = label || null;
      }
      // Every other directive (#PLAYLIST, #EXTGRP, comments) is metadata we
      // deliberately ignore rather than treat as a track.
      return;
    }

    if (isRemoteUrl(value) && !/^file:\/\//i.test(value)) {
      // A stream URL cannot be matched to a track by text, and following it
      // would mean fetching an arbitrary endpoint. Report it instead.
      remoteEntries += 1;
      skipped.push({ lineNumber, raw: value, reason: "跳过流媒体地址" });
      pendingTitle = null;
      return;
    }

    const verdict = classifyLocalPath(value);
    // With a real file we prefer its own tags; EXTINF text is only a fallback.
    const text = pendingTitle ?? fileNameStem(value);
    if (verdict.kind === "skip") {
      // Still importable by text, so it stays an entry and is also reported.
      refusedPaths.push({ lineNumber, raw: value, reason: verdict.reason });
    }
    const localPath = verdict.kind === "import" ? verdict.path : null;
    if (!localPath && !text) {
      skipped.push({ lineNumber, raw: value, reason: "无法识别的条目" });
      pendingTitle = null;
      return;
    }
    entries.push({ lineNumber, text, localPath });
    pendingTitle = null;
  });

  if (remoteEntries) notes.push(`已跳过 ${remoteEntries} 条流媒体地址。`);
  const localCount = entries.filter((entry) => entry.localPath).length;
  if (localCount) notes.push(`${localCount} 条指向本地文件，将直接读取文件标签导入。`);
  for (const reason of new Set(refusedPaths.map((entry) => entry.reason))) {
    const count = refusedPaths.filter((entry) => entry.reason === reason).length;
    notes.push(`${reason}（${count} 条），这些条目改为按歌名匹配。`);
  }
  return { format: "m3u", confidence: 1, entries, skipped, notes };
}

/** "…/Artist - Title.flac" -> "Artist - Title", so a bare path is still searchable. */
function fileNameStem(value: string): string {
  const name = value.split(/[\\/]/).pop() ?? value;
  return name.replace(/\.[a-z0-9]{2,5}$/i, "").trim();
}

function normalizeCsv(lines: readonly string[], delimiter: string): NormalizedPlaylist {
  const entries: NormalizedPlaylistEntry[] = [];
  const skipped: SkippedPlaylistLine[] = [];
  const notes: string[] = [];

  const rows = lines.map((line, index) => ({ lineNumber: index + 1, raw: line.trim() }))
    .filter((row) => row.raw.length > 0);
  if (!rows.length) return { format: "csv", confidence: 0, entries, skipped, notes };

  const firstFields = splitDelimited(rows[0].raw, delimiter);
  let titleColumn = headerIndex(firstFields, TITLE_HEADERS);
  let artistColumn = headerIndex(firstFields, ARTIST_HEADERS);
  const hasHeader = titleColumn >= 0 || artistColumn >= 0;
  if (hasHeader) {
    notes.push(`已识别表头：第 ${titleColumn + 1} 列为歌名${artistColumn >= 0 ? `，第 ${artistColumn + 1} 列为歌手` : ""}。`);
  } else {
    // No header: assume the conventional first-two-columns layout.
    titleColumn = 0;
    artistColumn = firstFields.length > 1 ? 1 : -1;
    notes.push("未发现表头，按前两列为歌名与歌手处理。");
  }

  rows.slice(hasHeader ? 1 : 0).forEach((row) => {
    const fields = splitDelimited(row.raw, delimiter);
    const title = (fields[titleColumn] ?? "").trim();
    const artist = artistColumn >= 0 ? (fields[artistColumn] ?? "").trim() : "";
    if (!title) {
      skipped.push({ lineNumber: row.lineNumber, raw: row.raw, reason: "缺少歌名" });
      return;
    }
    entries.push({
      lineNumber: row.lineNumber,
      text: artist ? `${title} - ${artist}` : title,
      localPath: null,
    });
  });

  return { format: "csv", confidence: 1, entries, skipped, notes };
}

function normalizePlain(lines: readonly string[]): NormalizedPlaylist {
  const entries: NormalizedPlaylistEntry[] = [];
  lines.forEach((rawLine, index) => {
    const value = (index === 0 ? rawLine.replace(/^\uFEFF/, "") : rawLine).trim();
    if (!value) return;
    entries.push({ lineNumber: index + 1, text: value, localPath: null });
  });
  return { format: "plain", confidence: 1, entries, skipped: [], notes: [] };
}

/**
 * Detect the format and reduce it to entries. `confidence` reflects how sure the
 * detection was, so the caller can surface a "looks like X" note; the parse
 * itself is exact once a format is chosen.
 */
export function normalizePlaylistText(text: string): NormalizedPlaylist {
  const allLines = splitLines(text);
  const lines = allLines.slice(0, MAX_PLAYLIST_INPUT_LINES);
  const truncated = allLines.length - lines.length;
  const detected = detectFormat(lines);

  let result: NormalizedPlaylist;
  if (detected.format === "m3u") result = normalizeM3u(lines);
  else if (detected.format === "csv") {
    const meaningful = lines.map((line) => line.trim()).filter(Boolean);
    const delimiter = detectDelimiter(meaningful)?.delimiter ?? ",";
    result = normalizeCsv(lines, delimiter);
  } else result = normalizePlain(lines);

  const notes = [...result.notes];
  if (truncated > 0) notes.push(`输入超过 ${MAX_PLAYLIST_INPUT_LINES} 行，已忽略末尾 ${truncated} 行。`);
  return { ...result, confidence: detected.confidence, notes };
}

export type PlaylistExportTrack = {
  title: string;
  artist: string;
  durationMs?: number | null;
  path?: string | null;
};

/**
 * Write an extended m3u8. Entries without a local path still get an #EXTINF so
 * the file stays a readable record, with the query text as the URI line — which
 * is what other players do for unresolved entries.
 */
export function buildM3u8(tracks: readonly PlaylistExportTrack[], playlistName?: string): string {
  const lines = ["#EXTM3U"];
  if (playlistName?.trim()) lines.push(`#PLAYLIST:${sanitizeDirectiveText(playlistName)}`);
  for (const track of tracks) {
    const seconds = typeof track.durationMs === "number" && Number.isFinite(track.durationMs) && track.durationMs > 0
      ? Math.round(track.durationMs / 1000)
      : -1;
    const label = track.artist
      ? `${sanitizeDirectiveText(track.artist)} - ${sanitizeDirectiveText(track.title)}`
      : sanitizeDirectiveText(track.title);
    lines.push(`#EXTINF:${seconds},${label}`);
    lines.push(track.path?.trim() || `${sanitizeDirectiveText(track.title)}${track.artist ? ` - ${sanitizeDirectiveText(track.artist)}` : ""}`);
  }
  return `${lines.join("\n")}\n`;
}

/** Newlines would forge a directive or entry; collapse them instead. */
function sanitizeDirectiveText(value: string): string {
  return value.replace(/[\r\n]+/g, " ").trim();
}

/** Text the existing importer consumes, for entries with no local file. */
export function toImportableText(entries: readonly NormalizedPlaylistEntry[]): string {
  return entries.filter((entry) => !entry.localPath).map((entry) => entry.text).join("\n");
}

/**
 * Dedup key matching local_path_key in the Rust host: Windows paths compare
 * case-insensitively with separators normalised, POSIX paths compare exactly.
 * The style is read from the path rather than the running platform, because a
 * playlist may have been written on a different OS than the one opening it.
 */
function localPathKey(path: string): string {
  const windowsStyle = /^[a-z]:[\\/]/i.test(path) || path.startsWith("\\\\");
  return windowsStyle ? path.replace(/\//g, "\\").toLowerCase() : path;
}

/** Local paths an m3u referenced, in input order and de-duplicated. */
export function toLocalPaths(entries: readonly NormalizedPlaylistEntry[]): string[] {
  const seen = new Set<string>();
  const paths: string[] = [];
  for (const entry of entries) {
    if (!entry.localPath) continue;
    const key = localPathKey(entry.localPath);
    if (seen.has(key)) continue;
    seen.add(key);
    paths.push(entry.localPath);
  }
  return paths;
}
