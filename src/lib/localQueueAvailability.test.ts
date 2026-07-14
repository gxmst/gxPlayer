import { describe, expect, it } from "vitest";
import type { PersistablePlaylistEntry } from "./playlistPersistence";
import {
  engineMatchesLocalQueue,
  localQueuePaths,
  relinkLocalQueuePath,
  unavailablePathsFromChecks,
} from "./localQueueAvailability";

const entries: PersistablePlaylistEntry[] = [
  {
    kind: "local",
    path: "D:\\Music\\missing.flac",
    title: "Missing",
    artist: "Artist",
    durationSeconds: 120,
  },
  {
    kind: "online",
    quality: "auto",
    track: {
      providerId: "demo",
      providerTrackId: "1",
      title: "Online",
      artist: "Artist",
      album: "",
      durationMs: null,
      artworkUrl: null,
      resolverPayload: {},
      preview: null,
    },
  },
  {
    kind: "local",
    path: "E:\\Music\\ready.flac",
    title: "Ready",
    artist: "Artist",
    durationSeconds: 180,
  },
];

describe("local queue availability", () => {
  it("checks restored local paths without removing or rewriting queue entries", () => {
    expect(localQueuePaths(entries)).toEqual([
      "D:\\Music\\missing.flac",
      "E:\\Music\\ready.flac",
    ]);
    expect([...unavailablePathsFromChecks(entries, [
      { path: "D:\\Music\\missing.flac", available: false },
      { path: "E:\\Music\\ready.flac", available: true },
    ])]).toEqual(["D:\\Music\\missing.flac"]);
    expect(entries).toHaveLength(3);
  });

  it("rejects incomplete backend results instead of treating unknown paths as missing", () => {
    expect(() => unavailablePathsFromChecks(entries, [
      { path: "E:\\Music\\ready.flac", available: true },
    ])).toThrow("本地路径检查结果不完整");
  });

  it("requires the engine queue to exactly match before using an in-place jump", () => {
    const localEntries = entries.filter(
      (entry): entry is Extract<PersistablePlaylistEntry, { kind: "local" }> => entry.kind === "local",
    );
    expect(engineMatchesLocalQueue(localEntries, [])).toBe(false);
    expect(engineMatchesLocalQueue(localEntries, [
      { location: localEntries[0]!.path, online: false },
      { location: localEntries[1]!.path, online: false },
    ])).toBe(true);
    expect(engineMatchesLocalQueue(localEntries, [
      { location: "E:\\Music\\relinked.flac", online: false },
      { location: localEntries[1]!.path, online: false },
    ])).toBe(false);
  });

  it("relinks every duplicate queue entry that still references the old path", () => {
    const old = entries[0] as Extract<PersistablePlaylistEntry, { kind: "local" }>;
    const replacement: Extract<PersistablePlaylistEntry, { kind: "local" }> = {
      ...old,
      path: "E:\\Music\\found.flac",
    };
    const next = relinkLocalQueuePath([old, entries[1]!, { ...old }], old.path, replacement);

    expect(next.map((entry) => entry.kind === "local" ? entry.path : entry.kind)).toEqual([
      replacement.path,
      "online",
      replacement.path,
    ]);
  });
});
