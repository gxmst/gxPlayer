import { describe, expect, it } from "vitest";
import type { HistoryEntry } from "../types";
import { groupConsecutiveHistory, historyEntryIdentity } from "./historyGrouping";

function historyEntry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    id: 1,
    playedAtMs: 1_700_000_000_000,
    kind: "online",
    title: "A",
    artist: "歌手",
    path: null,
    providerId: "provider",
    providerTrackId: "track-a",
    quality: "320k",
    ...overrides,
  };
}

describe("groupConsecutiveHistory", () => {
  it("只合并返回顺序中连续的同曲记录", () => {
    const entries = [
      historyEntry({ id: 1, title: "A", providerTrackId: "a" }),
      historyEntry({ id: 2, title: "A", providerTrackId: "a" }),
      historyEntry({ id: 3, title: "B", providerTrackId: "b" }),
      historyEntry({ id: 4, title: "B", providerTrackId: "b" }),
      historyEntry({ id: 5, title: "A", providerTrackId: "a" }),
    ];

    const groups = groupConsecutiveHistory(entries);

    expect(groups.map(({ entry, count }) => [entry.title, count])).toEqual([
      ["A", 2],
      ["B", 2],
      ["A", 1],
    ]);
    expect(groups[0]?.entry).toBe(entries[0]);
  });

  it("本地路径按 Windows 斜杠和大小写不敏感匹配", () => {
    const groups = groupConsecutiveHistory([
      historyEntry({ id: 1, kind: "local", path: "C:\\Music\\Album\\Song.flac", providerId: null, providerTrackId: null }),
      historyEntry({ id: 2, kind: "local", path: "c:/music/album/song.FLAC", providerId: null, providerTrackId: null }),
    ]);

    expect(groups).toHaveLength(1);
    expect(groups[0]?.count).toBe(2);
  });

  it("在线与缓存记录按 providerId 和 providerTrackId 合并并忽略 kind 与 quality", () => {
    const groups = groupConsecutiveHistory([
      historyEntry({ id: 1, kind: "online", quality: "128k" }),
      historyEntry({ id: 2, kind: "cached", quality: "flac" }),
    ]);

    expect(groups).toHaveLength(1);
    expect(groups[0]?.count).toBe(2);
  });

  it("没有稳定 ID 时回退到规范化后的标题和歌手", () => {
    const first = historyEntry({ providerId: null, providerTrackId: null, title: "  Song  Name ", artist: "Artist" });
    const second = historyEntry({ id: 2, providerId: null, providerTrackId: null, title: "song name", artist: " artist " });

    expect(historyEntryIdentity(first)).toBe(historyEntryIdentity(second));
    expect(groupConsecutiveHistory([first, second])[0]?.count).toBe(2);
  });

  it("空输入返回空分组", () => {
    expect(groupConsecutiveHistory([])).toEqual([]);
  });
});
