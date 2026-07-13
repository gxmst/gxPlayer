import type { CatalogTrack } from "../types";

export const DEFAULT_TEXT_PLAYLIST_MAX_LINES = 200;
export const DEFAULT_TEXT_PLAYLIST_MAX_LINE_LENGTH = 200;

export type ParsedTextPlaylistLine = {
  lineNumber: number;
  raw: string;
  query: string;
  title: string;
  artist: string;
  key: string;
};

export type RejectedTextPlaylistLine = {
  lineNumber: number;
  raw: string;
  reason: string;
};

export type TextPlaylistParseOptions = {
  maxLines?: number;
  maxLineLength?: number;
};

export type TextPlaylistParseResult = {
  lines: ParsedTextPlaylistLine[];
  rejected: RejectedTextPlaylistLine[];
  blankLines: number;
  truncatedLines: number;
  warnings: string[];
};

/** Normalize only for matching/deduplication; the original text remains visible to the user. */
export function normalizeTextPlaylistQuery(value: string): string {
  return value
    .normalize("NFKC")
    .trim()
    .toLowerCase()
    .replace(/\s+/g, " ");
}

function splitTitleAndArtist(value: string): { title: string; artist: string } {
  // Delimiters require surrounding whitespace so a hyphen inside a title is preserved.
  const separator = /\s+[-–—]\s+/g;
  let last: RegExpExecArray | null = null;
  let match: RegExpExecArray | null;
  while ((match = separator.exec(value)) !== null) last = match;

  if (!last || last.index === undefined) return { title: value, artist: "" };
  const title = value.slice(0, last.index).trim();
  const artist = value.slice(last.index + last[0].length).trim();
  if (!title || !artist) return { title: value, artist: "" };
  return { title, artist };
}

function isGenericUrl(value: string): boolean {
  return /^https?:\/\//i.test(value);
}

/**
 * Parse a user-pasted text list without interpreting any platform-specific format.
 * Empty lines are ignored; rejected lines are returned for visible feedback.
 */
export function parseTextPlaylist(
  text: string,
  options: TextPlaylistParseOptions = {},
): TextPlaylistParseResult {
  const maxLines = Math.max(1, Math.floor(options.maxLines ?? DEFAULT_TEXT_PLAYLIST_MAX_LINES));
  const maxLineLength = Math.max(1, Math.floor(options.maxLineLength ?? DEFAULT_TEXT_PLAYLIST_MAX_LINE_LENGTH));
  const rawLines = text.replace(/\r\n?/g, "\n").split("\n");
  const lines: ParsedTextPlaylistLine[] = [];
  const rejected: RejectedTextPlaylistLine[] = [];
  let blankLines = 0;
  let truncatedLines = 0;

  rawLines.forEach((rawLine, index) => {
    const lineNumber = index + 1;
    const value = (index === 0 ? rawLine.replace(/^\uFEFF/, "") : rawLine).trim();
    if (!value) {
      blankLines += 1;
      return;
    }
    if (lines.length + rejected.length >= maxLines) {
      truncatedLines += 1;
      return;
    }
    if (isGenericUrl(value)) {
      rejected.push({ lineNumber, raw: value, reason: "不支持链接格式，请输入歌曲文本" });
      return;
    }
    if (value.length > maxLineLength) {
      rejected.push({ lineNumber, raw: value, reason: `单行超过 ${maxLineLength} 个字符` });
      return;
    }

    const { title, artist } = splitTitleAndArtist(value);
    const query = artist ? `${title} ${artist}` : title;
    lines.push({
      lineNumber,
      raw: value,
      query,
      title,
      artist,
      key: normalizeTextPlaylistQuery(query),
    });
  });

  const warnings: string[] = [];
  if (rejected.length) warnings.push(`${rejected.length} 行无法匹配，请检查列表内容。`);
  if (truncatedLines) warnings.push(`已忽略超过上限的 ${truncatedLines} 行。`);
  return { lines, rejected, blankLines, truncatedLines, warnings };
}

function similarity(requested: string, candidate: string): number {
  const left = normalizeTextPlaylistQuery(requested);
  const right = normalizeTextPlaylistQuery(candidate);
  if (!left || !right) return 0;
  if (left === right) return 1;
  if (right.includes(left) || left.includes(right)) return 0.72;

  const requestedTokens = new Set(left.split(" ").filter(Boolean));
  const candidateTokens = right.split(" ").filter(Boolean);
  if (!requestedTokens.size || !candidateTokens.length) return 0;
  const overlap = candidateTokens.filter((token) => requestedTokens.has(token)).length;
  return overlap / Math.max(requestedTokens.size, candidateTokens.length) * 0.55;
}

/** Exposed for deterministic tests and future UI explanations. */
export function scoreCatalogCandidate(
  line: ParsedTextPlaylistLine,
  candidate: CatalogTrack,
): number {
  const titleScore = similarity(line.title, candidate.title);
  const artistScore = line.artist ? similarity(line.artist, candidate.artist) : 0;
  const queryScore = similarity(line.query, `${candidate.title} ${candidate.artist}`);
  if (line.artist) return titleScore * 0.7 + artistScore * 0.25 + queryScore * 0.05;
  return titleScore * 0.8 + queryScore * 0.2;
}

/** Pick the strongest textual match while preserving input order for ties. */
export function chooseCatalogMatch(
  line: ParsedTextPlaylistLine,
  candidates: readonly CatalogTrack[],
): CatalogTrack | null {
  let best: CatalogTrack | null = null;
  let bestScore = -1;
  candidates.forEach((candidate) => {
    const score = scoreCatalogCandidate(line, candidate);
    if (score > bestScore) {
      best = candidate;
      bestScore = score;
    }
  });
  return best;
}
