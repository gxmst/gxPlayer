import { describe, expect, it } from "vitest";
import {
  PLAYLIST_SESSION_STORAGE_KEY,
  clearPlaylistSession,
  loadPlaylistSession,
  savePlaylistSession,
  type PlaylistSessionState,
} from "./playlistPersistence";

class MemoryStorage {
  private readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

function mixedSession(): PlaylistSessionState {
  return {
    playlist: [
      {
        kind: "local",
        path: "C:\\Music\\local.flac",
        title: "Local",
        artist: "Artist",
        durationSeconds: 183.5,
      },
      {
        kind: "online",
        quality: "flac",
        track: {
          providerId: "wy",
          providerTrackId: "42",
          title: "Online",
          artist: "Singer",
          album: "Album",
          durationMs: 205_000,
          artworkUrl: "https://img.example/cover.jpg",
          resolverPayload: { source: "wy", musicInfo: { songmid: "42" } },
          preview: { url: "https://audio.example/preview.mp3", headers: [] },
        },
      },
      {
        kind: "cached",
        providerId: "kw",
        providerTrackId: "7",
        quality: "320k",
        title: "Cached",
        artist: "Singer",
      },
    ],
    currentIndex: 1,
    playMode: "repeat_all",
  };
}

describe("playlist persistence", () => {
  it("round-trips the logical queue while stripping resolved media requests", () => {
    const storage = new MemoryStorage();
    const state = mixedSession();

    expect(savePlaylistSession(state, storage)).toBe(true);
    const raw = storage.getItem(PLAYLIST_SESSION_STORAGE_KEY);
    expect(raw).not.toContain("audio.example/preview.mp3");
    expect(state.playlist[1]?.kind === "online" && state.playlist[1].track.preview).not.toBeNull();

    const restored = loadPlaylistSession(storage);
    expect(restored).toEqual({
      ...state,
      playlist: [
        state.playlist[0],
        {
          ...state.playlist[1],
          track: {
            ...(state.playlist[1]?.kind === "online" ? state.playlist[1].track : {}),
            preview: null,
          },
        },
        state.playlist[2],
      ],
    });
  });

  it("restores an empty queue and its playback mode", () => {
    const storage = new MemoryStorage();
    expect(savePlaylistSession({
      playlist: [],
      currentIndex: null,
      playMode: "shuffle",
    }, storage)).toBe(true);

    expect(loadPlaylistSession(storage)).toEqual({
      playlist: [],
      currentIndex: null,
      playMode: "shuffle",
    });
  });

  it("discards malformed, unsupported, or unsafe stored data", () => {
    const storage = new MemoryStorage();
    const invalidValues = [
      "not json",
      JSON.stringify({ version: 2, playlist: [], currentIndex: null, playMode: "sequential" }),
      JSON.stringify({ version: 1, playlist: [], currentIndex: 0, playMode: "sequential" }),
      JSON.stringify({ version: 1, playlist: [], currentIndex: null, playMode: "invalid" }),
      JSON.stringify({
        version: 1,
        playlist: [mixedSession().playlist[1]],
        currentIndex: 0,
        playMode: "sequential",
      }),
    ];

    for (const raw of invalidValues) {
      storage.setItem(PLAYLIST_SESSION_STORAGE_KEY, raw);
      expect(loadPlaylistSession(storage)).toEqual({
        playlist: [],
        currentIndex: null,
        playMode: "sequential",
      });
      expect(storage.getItem(PLAYLIST_SESSION_STORAGE_KEY)).toBeNull();
    }
  });

  it("contains storage failures and exposes an explicit clear operation", () => {
    const storage = new MemoryStorage();
    expect(savePlaylistSession(mixedSession(), storage)).toBe(true);
    expect(clearPlaylistSession(storage)).toBe(true);
    expect(storage.getItem(PLAYLIST_SESSION_STORAGE_KEY)).toBeNull();

    const brokenStorage = {
      getItem: () => { throw new Error("blocked"); },
      setItem: () => { throw new Error("quota"); },
      removeItem: () => { throw new Error("blocked"); },
    };
    expect(loadPlaylistSession(brokenStorage)).toEqual({
      playlist: [],
      currentIndex: null,
      playMode: "sequential",
    });
    expect(savePlaylistSession(mixedSession(), brokenStorage)).toBe(false);
    expect(clearPlaylistSession(brokenStorage)).toBe(false);
  });

  it("keeps an unavailable restored local path in persistent storage", () => {
    const storage = new MemoryStorage();
    const state = mixedSession();

    expect(savePlaylistSession(state, storage)).toBe(true);
    expect(loadPlaylistSession(storage)).toEqual({
      ...state,
      playlist: [
        state.playlist[0],
        {
          ...state.playlist[1],
          track: {
            ...(state.playlist[1]?.kind === "online" ? state.playlist[1].track : {}),
            preview: null,
          },
        },
        state.playlist[2],
      ],
    });
  });
});
