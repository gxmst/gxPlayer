import { describe, expect, it } from "vitest";
import {
  buildM3u8,
  MAX_PLAYLIST_INPUT_LINES,
  normalizePlaylistText,
  PLAYLIST_DETECTION_CONFIDENCE_THRESHOLD,
  toImportableText,
  toLocalPaths,
} from "./playlistFormats";

describe("normalizePlaylistText", () => {
  it("reads an extended m3u and pairs EXTINF titles with their entries", () => {
    const result = normalizePlaylistText(
      [
        "#EXTM3U",
        "#PLAYLIST:Road Trip",
        "#EXTINF:213,Daft Punk - Around the World",
        "C:\\Music\\around.flac",
        "#EXTINF:-1,Unknown Length",
        "D:\\Music\\unknown.mp3",
      ].join("\n"),
    );

    expect(result.format).toBe("m3u");
    expect(result.confidence).toBe(1);
    expect(result.entries).toEqual([
      { lineNumber: 4, text: "Daft Punk - Around the World", localPath: "C:\\Music\\around.flac" },
      { lineNumber: 6, text: "Unknown Length", localPath: "D:\\Music\\unknown.mp3" },
    ]);
  });

  it("skips stream URLs instead of fetching them", () => {
    const result = normalizePlaylistText(
      ["#EXTM3U", "#EXTINF:-1,Live Radio", "https://example.invalid/stream.m3u8", "#EXTINF:180,Local - Song", "/home/u/song.ogg"].join("\n"),
    );

    expect(result.entries).toEqual([
      { lineNumber: 5, text: "Local - Song", localPath: "/home/u/song.ogg" },
    ]);
    expect(result.skipped).toEqual([
      { lineNumber: 3, raw: "https://example.invalid/stream.m3u8", reason: "跳过流媒体地址" },
    ]);
    expect(result.notes).toContain("已跳过 1 条流媒体地址。");
  });

  it("recognises a bare path list as m3u and derives text from the file name", () => {
    const result = normalizePlaylistText(
      ["C:\\Music\\Radiohead - Creep.flac", "C:\\Music\\Oasis - Wonderwall.mp3", "C:\\Music\\Blur - Song 2.mp3"].join("\n"),
    );

    expect(result.format).toBe("m3u");
    expect(result.confidence).toBeGreaterThanOrEqual(PLAYLIST_DETECTION_CONFIDENCE_THRESHOLD);
    expect(result.entries.map((entry) => entry.text)).toEqual([
      "Radiohead - Creep",
      "Oasis - Wonderwall",
      "Blur - Song 2",
    ]);
  });

  it("decodes file:// URIs into local paths", () => {
    const result = normalizePlaylistText(
      ["#EXTM3U", "#EXTINF:1,A - B", "file:///C:/Music/a%20b.flac", "#EXTINF:1,C - D", "file:///home/u/c%20d.mp3"].join("\n"),
    );

    expect(toLocalPaths(result.entries)).toEqual(["C:/Music/a b.flac", "/home/u/c d.mp3"]);
  });

  it("uses a CSV header to find the columns regardless of order", () => {
    const result = normalizePlaylistText(
      ["Artist,Title,Album", "Daft Punk,Around the World,Homework", "Air,La Femme d'Argent,Moon Safari"].join("\n"),
    );

    expect(result.format).toBe("csv");
    expect(result.entries.map((entry) => entry.text)).toEqual([
      "Around the World - Daft Punk",
      "La Femme d'Argent - Air",
    ]);
  });

  it("honours quoted CSV fields containing the delimiter", () => {
    const result = normalizePlaylistText(
      ['Title,Artist', '"Song, With Comma","Artist ""Nickname"""', "Plain,Other"].join("\n"),
    );

    expect(result.entries.map((entry) => entry.text)).toEqual([
      'Song, With Comma - Artist "Nickname"',
      "Plain - Other",
    ]);
  });

  it("does not treat a single comma in one title as a table", () => {
    const result = normalizePlaylistText(
      ["Hey Jude - The Beatles", "Wait, Wait - Somebody", "Creep - Radiohead", "Song 2 - Blur"].join("\n"),
    );

    expect(result.format).toBe("plain");
    expect(result.entries).toHaveLength(4);
    expect(result.entries[1].text).toBe("Wait, Wait - Somebody");
  });

  it("keeps plain lines untouched so the text importer splits them", () => {
    const result = normalizePlaylistText("  Creep - Radiohead  \n\n海阔天空 - Beyond\n");

    expect(result.format).toBe("plain");
    expect(result.entries).toEqual([
      { lineNumber: 1, text: "Creep - Radiohead", localPath: null },
      { lineNumber: 3, text: "海阔天空 - Beyond", localPath: null },
    ]);
  });

  it("reports zero confidence for empty input rather than guessing", () => {
    const result = normalizePlaylistText("\n\n   \n");
    expect(result.confidence).toBe(0);
    expect(result.entries).toEqual([]);
  });

  it("bounds absurd input and says how much it dropped", () => {
    const lines = Array.from({ length: MAX_PLAYLIST_INPUT_LINES + 5 }, (_, index) => `Song ${index} - Artist`);
    const result = normalizePlaylistText(lines.join("\n"));

    expect(result.entries).toHaveLength(MAX_PLAYLIST_INPUT_LINES);
    expect(result.notes.some((note) => note.includes("已忽略末尾 5 行"))).toBe(true);
  });

  it("ignores unknown directives without turning them into tracks", () => {
    const result = normalizePlaylistText(
      ["#EXTM3U", "#EXTGRP:Rock", "# a stray comment", "#EXTINF:1,A - B", "C:\\a.flac"].join("\n"),
    );

    expect(result.entries).toHaveLength(1);
    expect(result.skipped).toEqual([]);
  });
});

describe("toImportableText", () => {
  it("emits only entries that still need an online match", () => {
    const result = normalizePlaylistText(
      ["#EXTM3U", "#EXTINF:1,Local - Song", "C:\\a.flac", "#EXTINF:1,Remote Only - Artist", "Remote Only - Artist"].join("\n"),
    );

    expect(toImportableText(result.entries)).toBe("Remote Only - Artist");
  });
});

describe("buildM3u8", () => {
  it("writes durations in seconds and keeps local paths as the URI", () => {
    const text = buildM3u8(
      [
        { title: "Around the World", artist: "Daft Punk", durationMs: 213_400, path: "C:\\Music\\a.flac" },
        { title: "No Duration", artist: "Someone", durationMs: null, path: null },
      ],
      "Road Trip",
    );

    expect(text).toBe(
      [
        "#EXTM3U",
        "#PLAYLIST:Road Trip",
        "#EXTINF:213,Daft Punk - Around the World",
        "C:\\Music\\a.flac",
        "#EXTINF:-1,Someone - No Duration",
        "No Duration - Someone",
        "",
      ].join("\n"),
    );
  });

  it("collapses newlines so a title cannot forge a directive", () => {
    const text = buildM3u8([
      { title: "Evil\n#EXTINF:1,Injected", artist: "A\nB", durationMs: 1_000, path: null },
    ]);

    expect(text.split("\n").filter((line) => line.startsWith("#EXTINF"))).toEqual([
      "#EXTINF:1,A B - Evil #EXTINF:1,Injected",
    ]);
  });

  it("round-trips through the parser", () => {
    const text = buildM3u8([
      { title: "Creep", artist: "Radiohead", durationMs: 238_000, path: "C:\\Music\\creep.flac" },
    ]);
    const parsed = normalizePlaylistText(text);

    // No playlist name was given, so there is no #PLAYLIST line: header, EXTINF, URI.
    expect(parsed.format).toBe("m3u");
    expect(parsed.entries).toEqual([
      { lineNumber: 3, text: "Radiohead - Creep", localPath: "C:\\Music\\creep.flac" },
    ]);
  });
});
