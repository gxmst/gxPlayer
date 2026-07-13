import { describe, expect, it } from "vitest";
import type { CatalogTrack } from "../types";
import {
  chooseCatalogMatch,
  normalizeTextPlaylistQuery,
  parseTextPlaylist,
  scoreCatalogCandidate,
} from "./textPlaylistImport";

function track(title: string, artist = "歌手", providerId = "provider"): CatalogTrack {
  return {
    providerId,
    providerTrackId: `${providerId}:${title}`,
    title,
    artist,
    album: "专辑",
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}

describe("text playlist parsing", () => {
  it("normalizes line endings, keeps line numbers, and splits the spaced artist delimiter", () => {
    const result = parseTextPlaylist("\uFEFF歌名 - 歌手\r\n\r\n带 - 破折号 - 另一位\n纯歌名");

    expect(result.lines).toEqual([
      expect.objectContaining({
        lineNumber: 1,
        raw: "歌名 - 歌手",
        title: "歌名",
        artist: "歌手",
        query: "歌名 歌手",
      }),
      expect.objectContaining({
        lineNumber: 3,
        title: "带 - 破折号",
        artist: "另一位",
        query: "带 - 破折号 另一位",
      }),
      expect.objectContaining({ lineNumber: 4, title: "纯歌名", artist: "", query: "纯歌名" }),
    ]);
    expect(result.blankLines).toBe(1);
    expect(result.rejected).toEqual([]);
  });

  it("rejects generic links and bounds oversized input without interpreting a platform", () => {
    const result = parseTextPlaylist(
      ["歌曲", "https://example.invalid/list", "太长".repeat(4), "第三首", "第四首"].join("\n"),
      { maxLines: 4, maxLineLength: 5 },
    );

    expect(result.lines.map((line) => line.raw)).toEqual(["歌曲", "第三首"]);
    expect(result.rejected).toEqual([
      expect.objectContaining({ lineNumber: 2, reason: "不支持链接格式，请输入歌曲文本" }),
      expect.objectContaining({ lineNumber: 3, reason: "单行超过 5 个字符" }),
    ]);
    expect(result.truncatedLines).toBe(1);
    expect(result.warnings).toHaveLength(2);
  });

  it("normalizes only matching keys, preserving the visible text", () => {
    const result = parseTextPlaylist("  Hello   World  ");
    expect(result.lines[0]?.raw).toBe("Hello   World");
    expect(result.lines[0]?.key).toBe("hello world");
    expect(normalizeTextPlaylistQuery("Ｈｅｌｌｏ  WORLD")).toBe("hello world");
  });
});

describe("text playlist candidate ranking", () => {
  it("prefers exact title and artist over the first returned candidate", () => {
    const line = parseTextPlaylist("目标歌 - 目标歌手").lines[0]!;
    const weak = track("目标歌现场版", "其他歌手", "fast");
    const exact = track("目标歌", "目标歌手", "slow");

    expect(scoreCatalogCandidate(line, exact)).toBeGreaterThan(scoreCatalogCandidate(line, weak));
    expect(chooseCatalogMatch(line, [weak, exact])).toBe(exact);
  });

  it("uses the first candidate for a textual tie and returns null for no results", () => {
    const line = parseTextPlaylist("没有歌手").lines[0]!;
    const first = track("没有歌手", "甲", "first");
    const second = track("没有歌手", "乙", "second");

    expect(chooseCatalogMatch(line, [first, second])).toBe(first);
    expect(chooseCatalogMatch(line, [])).toBeNull();
  });
});
