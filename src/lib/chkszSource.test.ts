import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";

type SourceHandler = (payload: Record<string, unknown>) => Promise<unknown>;

const sourceScript = readFileSync(
  new URL("../../sources/chksz-api.js", import.meta.url),
  "utf8",
);

afterEach(() => {
  delete (globalThis as Record<string, unknown>).ls;
  delete (globalThis as Record<string, unknown>).lx;
});

describe("ChKSz protocol-v2 source", () => {
  it("advertises optional actions, normalizes responses and caches identical searches", async () => {
    let handler: SourceHandler | null = null;
    let capabilities: unknown = null;
    const requests: URL[] = [];
    (globalThis as Record<string, unknown>).ls = {
      api: {
        addr: "https://api.chksz.com/",
        pass: "chksz_test_key",
        searchProvider: "wy",
      },
    };
    (globalThis as Record<string, unknown>).lx = {
      on: (_event: string, registered: SourceHandler) => {
        handler = registered;
        return Promise.resolve();
      },
      send: (_event: string, payload: unknown) => {
        capabilities = payload;
        return Promise.resolve();
      },
      request: (
        rawUrl: string,
        _options: unknown,
        callback: (error: Error | null, response?: unknown, body?: unknown) => void,
      ) => {
        const url = new URL(rawUrl);
        requests.push(url);
        const responses: Record<string, unknown> = {
          "/api/163_search": {
            result: {
              songs: [{
                id: 42,
                name: "Result",
                ar: [{ name: "Artist" }],
                al: { id: 7, name: "Album", picUrl: "https://image.example/cover.jpg" },
                dt: 180_000,
              }],
            },
          },
          "/api/163_lyric": {
            lrc: { lyric: "[00:01.00]Original" },
            tlyric: { lyric: "[00:01.00]Translation" },
            romalrc: { lyric: "[00:01.00]Romanization" },
          },
          "/api/163_playlist": {
            playlist: {
              name: "Remote list",
              creator: { nickname: "Owner" },
              tracks: [{ id: 42, name: "Result", ar: [{ name: "Artist" }], al: { name: "Album" } }],
            },
          },
          "/api/163_music": "https://media.example/song.flac",
        };
        callback(null, { statusCode: 200 }, responses[url.pathname]);
      },
    };

    (0, eval)(sourceScript);
    const sourceHandler = handler as SourceHandler | null;
    expect(sourceHandler).not.toBeNull();
    expect(capabilities).toMatchObject({
      protocolVersion: 2,
      actions: ["musicUrl", "search", "lyric", "playlist"],
    });

    const searchPayload = { action: "search", info: { keyword: "Result", limit: 10, offset: 0 } };
    const first = await sourceHandler!(searchPayload);
    const second = await sourceHandler!(searchPayload);
    expect(first).toEqual(second);
    expect(first).toMatchObject({
      tracks: [{ providerId: "wy", providerTrackId: "42", title: "Result" }],
    });

    const lyric = await sourceHandler!({
      action: "lyric",
      source: "wy",
      info: { musicInfo: { songmid: "42" } },
    });
    expect(lyric).toMatchObject({
      lines: [{
        timestampMs: 1000,
        text: "Original",
        translation: "Translation",
        romanization: "Romanization",
      }],
    });

    const playlist = await sourceHandler!({
      action: "playlist",
      source: "wy",
      info: { id: "99" },
    });
    expect(playlist).toMatchObject({
      id: "99",
      name: "Remote list",
      tracks: [{ providerId: "wy", providerTrackId: "42" }],
    });

    const media = await sourceHandler!({
      action: "musicUrl",
      source: "wy",
      info: { type: "flac", musicInfo: { songmid: "42" } },
    });
    expect(media).toBe("https://media.example/song.flac");
    expect(requests.map((url) => url.pathname)).toEqual([
      "/api/163_search",
      "/api/163_lyric",
      "/api/163_playlist",
      "/api/163_music",
    ]);
    expect(requests.every((url) => url.searchParams.get("apikey") === "chksz_test_key")).toBe(true);
  });
});
