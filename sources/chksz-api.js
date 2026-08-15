/*!
 * @name ChKSz API
 * @version 2.0.0
 * @author GXPlayer
 * @homepage https://api.chksz.com
 * @description ChKSz protocol-v2 source for search, lyrics, playlists and playback.
 */

(() => {
  const apiConfig = globalThis.ls?.api ?? {};
  const baseUrl = String(apiConfig.addr || "https://api.chksz.com").replace(/\/+$/, "");
  const apiKey = String(apiConfig.pass || "").trim();
  const searchProvider = ["wy", "tx", "kg"].includes(apiConfig.searchProvider)
    ? apiConfig.searchProvider
    : "wy";
  const qualitys = ["128k", "320k", "flac", "flac24bit"];
  const cache = new Map();
  const sizes = { "128k": "128k", "320k": "320k", flac: "flac", flac24bit: "master" };
  const neteaseLevels = {
    "128k": "standard",
    "320k": "exhigh",
    flac: "lossless",
    flac24bit: "jymaster",
  };

  class ChkszError extends Error {
    constructor(status, message) {
      super(message);
      this.name = "ChkszError";
      this.status = status;
    }
  }

  function redactSensitive(value) {
    return String(value || "")
      .replace(/chksz_[A-Za-z0-9_-]+/g, "chksz_<redacted>")
      .replace(/([?&]apikey=)[^&\s"']+/gi, "$1<redacted>")
      .replace(/https?:\/\/[^\s"']+/gi, "<url>")
      .slice(0, 300);
  }

  function transportMessage(error) {
    const message = String(error || "").toLowerCase();
    if (message.includes("timeout") || message.includes("timed out")) return "ChKSz request timed out";
    if (message.includes("dns") || message.includes("resolve")) return "ChKSz DNS lookup failed";
    if (message.includes("tls") || message.includes("ssl") || message.includes("certificate")) {
      return "ChKSz TLS connection failed";
    }
    return "ChKSz request failed";
  }

  function requestUrl(path, params) {
    const query = new URLSearchParams();
    for (const [name, value] of Object.entries(params)) {
      if (value !== undefined && value !== null && String(value) !== "") {
        query.set(name, String(value));
      }
    }
    query.set("apikey", apiKey);
    return baseUrl + path + "?" + query.toString();
  }

  function bodyMessage(body) {
    if (body && typeof body === "object") {
      return redactSensitive(body.msg || body.message || body.error || "ChKSz returned no usable data");
    }
    const raw = String(body || "").trim();
    if (!raw) return "ChKSz returned an empty response";
    try {
      return bodyMessage(JSON.parse(raw));
    } catch {
      return redactSensitive(raw);
    }
  }

  function jsonBody(body) {
    if (body && typeof body === "object") return body;
    const raw = String(body || "").trim();
    if (!raw) return {};
    try {
      return JSON.parse(raw);
    } catch {
      return raw;
    }
  }

  function request(path, params) {
    return new Promise((resolve, reject) => {
      lx.request(
        requestUrl(path, params),
        {
          method: "GET",
          timeout: 15000,
          headers: { Accept: "application/json, text/plain" },
        },
        (error, response, body) => {
          if (error) {
            reject(new ChkszError(0, transportMessage(error)));
            return;
          }
          const status = Number(response?.statusCode || 0);
          if (status < 200 || status >= 300) {
            reject(new ChkszError(status, "ChKSz HTTP " + status + ": " + bodyMessage(body)));
            return;
          }
          resolve(jsonBody(body));
        },
      );
    });
  }

  function cached(key, ttlMs, loader) {
    const now = Date.now();
    const existing = cache.get(key);
    if (existing && existing.expiresAt > now) return existing.promise;
    const promise = Promise.resolve().then(loader);
    cache.set(key, { expiresAt: now + ttlMs, promise });
    promise.catch(() => {
      if (cache.get(key)?.promise === promise) cache.delete(key);
    });
    return promise;
  }

  function requireKey() {
    if (!apiKey) {
      throw new Error("ChKSz API Key is not configured; set lsConfig.api.pass in source settings");
    }
  }

  function requireId(info, field) {
    const value = String(info?.[field] || "").trim();
    if (!value) throw new Error("ChKSz source is missing musicInfo." + field);
    return value;
  }

  function playableUrl(body) {
    if (typeof body === "string") {
      const raw = body.trim();
      if (/^https?:\/\//i.test(raw)) return raw;
      try {
        return playableUrl(JSON.parse(raw));
      } catch {
        return null;
      }
    }
    if (body && typeof body === "object" && typeof body.url === "string") {
      return body.url.trim() || null;
    }
    return null;
  }

  async function requestPlayable(path, params) {
    const body = await request(path, { ...params, type: "text" });
    const resolved = playableUrl(body);
    if (!resolved) throw new ChkszError(200, bodyMessage(body));
    return resolved;
  }

  function arrayAt(...values) {
    return values.find(Array.isArray) || [];
  }

  function text(value) {
    return value === undefined || value === null ? "" : String(value).trim();
  }

  function durationMs(value) {
    if (typeof value === "number" && Number.isFinite(value)) {
      return value > 10_000 ? Math.round(value) : Math.round(value * 1000);
    }
    const raw = text(value);
    const match = raw.match(/^(\d+):(\d{1,2})$/);
    return match ? (Number(match[1]) * 60 + Number(match[2])) * 1000 : null;
  }

  function artwork(value) {
    const url = text(value).replace("{size}", "400").replace(/^http:\/\//i, "https://");
    return /^https?:\/\//i.test(url) ? url : null;
  }

  function artistNames(value) {
    if (Array.isArray(value)) {
      return value.map((artist) => text(artist?.name || artist)).filter(Boolean).join("、");
    }
    return text(value?.name || value);
  }

  function neteaseTrack(song) {
    const id = text(song?.id || song?.songmid);
    const title = text(song?.name || song?.title);
    if (!id || !title) return null;
    const album = song?.al || song?.album || {};
    const artist = artistNames(song?.ar || song?.artists || song?.artist);
    const albumName = text(album?.name || song?.albumName);
    const image = artwork(album?.picUrl || album?.pic_url || song?.cover || song?.picUrl);
    const intervalMs = durationMs(song?.dt ?? song?.duration ?? song?.interval);
    return {
      providerId: "wy",
      providerTrackId: id,
      title,
      artist,
      album: albumName,
      durationMs: intervalMs,
      artworkUrl: image,
      resolverPayload: {
        source: "wy",
        musicInfo: {
          source: "wy",
          songmid: id,
          name: title,
          singer: artist,
          albumName,
          albumId: text(album?.id || song?.albumId),
          img: image,
        },
      },
      preview: null,
    };
  }

  function qqTrack(song) {
    const id = text(song?.mid || song?.songmid || song?.id);
    const title = text(song?.name || song?.title);
    if (!id || !title) return null;
    const artist = artistNames(song?.singer || song?.artist);
    const albumName = text(song?.album?.name || song?.album);
    const image = artwork(song?.cover);
    return {
      providerId: "tx",
      providerTrackId: id,
      title,
      artist,
      album: albumName,
      durationMs: durationMs(song?.interval || song?.duration),
      artworkUrl: image,
      resolverPayload: {
        source: "tx",
        musicInfo: {
          source: "tx",
          songmid: id,
          name: title,
          singer: artist,
          albumName,
          img: image,
        },
      },
      preview: null,
    };
  }

  function kugouTrack(song) {
    const id = text(song?.id || song?.hash || song?.songmid);
    const title = text(song?.name || song?.title);
    if (!id || !title) return null;
    const artist = artistNames(song?.singer || song?.artist);
    const albumName = text(song?.album);
    const image = artwork(song?.cover);
    return {
      providerId: "kg",
      providerTrackId: id,
      title,
      artist,
      album: albumName,
      durationMs: durationMs(song?.duration || song?.interval),
      artworkUrl: image,
      resolverPayload: {
        source: "kg",
        musicInfo: {
          source: "kg",
          songmid: id,
          hash: id,
          name: title,
          singer: artist,
          albumName,
          img: image,
        },
      },
      preview: null,
    };
  }

  function searchTracks(provider, body) {
    if (provider === "wy") {
      const songs = arrayAt(
        body?.result?.songs,
        body?.data?.result?.songs,
        body?.data?.songs,
        body?.songs,
        body?.list,
      );
      return songs.map(neteaseTrack).filter(Boolean);
    }
    const list = arrayAt(body?.list, body?.data?.list, body?.data, body?.result?.list);
    return list.map(provider === "tx" ? qqTrack : kugouTrack).filter(Boolean);
  }

  async function search(info) {
    requireKey();
    const keyword = text(info?.keyword);
    const limit = Math.min(50, Math.max(1, Number(info?.limit) || 20));
    const offset = Math.max(0, Number(info?.offset) || 0);
    if (!keyword) return { tracks: [] };
    const key = ["search", searchProvider, keyword, limit, offset].join(":");
    return cached(key, 10 * 60_000, async () => {
      let body;
      if (searchProvider === "wy") {
        body = await request("/api/163_search", { keyword, limit, offset });
      } else if (searchProvider === "tx") {
        body = await request("/api/qq_music", { msg: keyword, num: limit });
      } else {
        body = await request("/api/kugou_music", { msg: keyword });
      }
      return { tracks: searchTracks(searchProvider, body).slice(0, limit) };
    });
  }

  function parseLrc(value) {
    const raw = text(value);
    if (!raw) return [];
    const lines = [];
    for (const rawLine of raw.split(/\r?\n/)) {
      const tags = [...rawLine.matchAll(/\[(\d+):(\d{1,2}(?:\.\d+)?)\]/g)];
      const lineText = rawLine.replace(/\[[^\]]+\]/g, "").trim();
      if (!lineText) continue;
      if (!tags.length) {
        lines.push({ timestampMs: null, text: lineText });
        continue;
      }
      for (const tag of tags) {
        lines.push({
          timestampMs: Math.round((Number(tag[1]) * 60 + Number(tag[2])) * 1000),
          text: lineText,
        });
      }
    }
    return lines.sort((left, right) => (left.timestampMs ?? 0) - (right.timestampMs ?? 0));
  }

  function lyricText(body, kind) {
    const root = body?.data || body || {};
    if (kind === "translation") return root?.tlyric?.lyric || root?.translation || root?.trans || "";
    if (kind === "romanization") {
      return root?.romalrc?.lyric || root?.roma || root?.romanization || "";
    }
    return root?.lrc?.lyric || root?.lyric || root?.lrc || "";
  }

  function lyricDocument(body) {
    const primary = parseLrc(lyricText(body, "primary"));
    const translations = new Map(
      parseLrc(lyricText(body, "translation"))
        .filter((line) => line.timestampMs !== null)
        .map((line) => [line.timestampMs, line.text]),
    );
    const romanizations = new Map(
      parseLrc(lyricText(body, "romanization"))
        .filter((line) => line.timestampMs !== null)
        .map((line) => [line.timestampMs, line.text]),
    );
    return {
      instrumental: false,
      lines: primary.map((line) => ({
        ...line,
        translation: line.timestampMs === null ? null : translations.get(line.timestampMs) || null,
        romanization: line.timestampMs === null ? null : romanizations.get(line.timestampMs) || null,
      })),
    };
  }

  async function lyric(payload) {
    requireKey();
    const source = text(payload?.source);
    const info = payload?.info?.musicInfo || {};
    const id = text(info.songmid || info.hash);
    if (!id || !["wy", "tx", "kg"].includes(source)) return null;
    return cached(["lyric", source, id].join(":"), 24 * 60 * 60_000, async () => {
      let body;
      if (source === "wy") {
        body = await request("/api/163_lyric", { id });
      } else if (source === "tx") {
        body = await request("/api/qq_music", { mid: id, size: "128k" });
      } else {
        body = await request("/api/kugou_music", { id: text(info.hash || id), size: "128k" });
      }
      return lyricDocument(body);
    });
  }

  async function playlist(info) {
    requireKey();
    const id = text(info?.id);
    if (!id) throw new Error("ChKSz playlist action requires info.id");
    return cached(["playlist", "wy", id].join(":"), 10 * 60_000, async () => {
      const body = await request("/api/163_playlist", { id });
      const root = body?.playlist || body?.data?.playlist || body?.data || body || {};
      const tracks = arrayAt(root?.tracks, root?.songs, root?.list)
        .map(neteaseTrack)
        .filter(Boolean)
        .slice(0, 2000);
      return {
        id,
        name: text(root?.name) || "NetEase playlist " + id,
        coverUrl: artwork(root?.coverImgUrl || root?.cover || root?.picUrl),
        creator: text(root?.creator?.nickname || root?.creator?.name || root?.creator),
        tracks,
      };
    });
  }

  async function musicUrl(payload) {
    requireKey();
    const source = text(payload?.source);
    const info = payload?.info?.musicInfo ?? {};
    const requestedQuality = text(payload?.info?.type) || "320k";
    const size = sizes[requestedQuality] || sizes["320k"];
    if (source === "wy") {
      return requestPlayable("/api/163_music", {
        id: requireId(info, "songmid"),
        level: neteaseLevels[requestedQuality] || neteaseLevels["320k"],
      });
    }
    if (source === "tx") {
      return requestPlayable("/api/qq_music", { mid: requireId(info, "songmid"), size });
    }
    if (source === "kg") {
      return requestPlayable("/api/kugou_music", {
        id: text(info.hash || requireId(info, "songmid")),
        size,
      });
    }
    throw new Error("ChKSz source does not support platform '" + source + "'");
  }

  lx.on("request", (payload) => {
    switch (payload?.action) {
      case "musicUrl":
        return musicUrl(payload);
      case "search":
        return search(payload?.info);
      case "lyric":
        return lyric(payload);
      case "playlist":
        return playlist(payload?.info);
      default:
        throw new Error("ChKSz source received unsupported action '" + text(payload?.action) + "'");
    }
  });

  void lx.send("inited", {
    status: true,
    protocolVersion: 2,
    actions: ["musicUrl", "search", "lyric", "playlist"],
    sources: {
      tx: {
        name: "QQ Music via ChKSz",
        type: "music",
        actions: ["musicUrl", "search", "lyric"],
        qualitys,
      },
      kg: {
        name: "Kugou via ChKSz",
        type: "music",
        actions: ["musicUrl", "search", "lyric"],
        qualitys,
      },
      wy: {
        name: "NetEase via ChKSz",
        type: "music",
        actions: ["musicUrl", "search", "lyric", "playlist"],
        qualitys,
      },
    },
  });
})();
