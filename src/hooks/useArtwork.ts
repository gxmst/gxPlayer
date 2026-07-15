import { useEffect, useState } from "react";
import { invoke } from "../lib/tauriClient";

const MAX_CACHE_ENTRIES = 96;
const MAX_CACHE_CHARACTERS = 24 * 1024 * 1024;
const safeDataUrl = /^data:image\/(?:jpeg|png|gif|webp);base64,/i;
const remoteUrl = /^https?:\/\//i;

type ArtworkPayload = {
  mime: string;
  dataUrl: string;
};

const cache = new Map<string, string>();
const pending = new Map<string, Promise<string | null>>();
let cachedCharacters = 0;

export function isRemoteArtworkUrl(url: string | null | undefined): url is string {
  return typeof url === "string" && remoteUrl.test(url);
}

function directArtworkUrl(url: string | null | undefined): string | null {
  return typeof url === "string" && safeDataUrl.test(url) ? url : null;
}

function remember(url: string, dataUrl: string) {
  const previous = cache.get(url);
  if (previous) {
    cachedCharacters -= previous.length;
    cache.delete(url);
  }
  cache.set(url, dataUrl);
  cachedCharacters += dataUrl.length;
  while (cache.size > MAX_CACHE_ENTRIES || cachedCharacters > MAX_CACHE_CHARACTERS) {
    const oldest = cache.entries().next().value as [string, string] | undefined;
    if (!oldest) break;
    cache.delete(oldest[0]);
    cachedCharacters -= oldest[1].length;
  }
}

function fetchArtwork(url: string): Promise<string | null> {
  const cached = cache.get(url);
  if (cached) {
    cache.delete(url);
    cache.set(url, cached);
    return Promise.resolve(cached);
  }
  const active = pending.get(url);
  if (active) return active;

  const request = invoke<ArtworkPayload>("artwork_get", { url })
    .then((payload) => {
      if (!safeDataUrl.test(payload.dataUrl)) return null;
      remember(url, payload.dataUrl);
      return payload.dataUrl;
    })
    .catch(() => null)
    .finally(() => {
      if (pending.get(url) === request) pending.delete(url);
    });
  pending.set(url, request);
  return request;
}

export function useArtworkUrl(url: string | null | undefined, enabled = true): string | null {
  const direct = directArtworkUrl(url);
  const remote = enabled && isRemoteArtworkUrl(url) ? url : null;
  const cached = remote ? cache.get(remote) ?? null : null;
  const [resolved, setResolved] = useState<{ source: string; dataUrl: string } | null>(null);

  useEffect(() => {
    if (!remote || cached) return;
    let disposed = false;
    void fetchArtwork(remote).then((dataUrl) => {
      if (!disposed && dataUrl) setResolved({ source: remote, dataUrl });
    });
    return () => {
      disposed = true;
    };
  }, [cached, remote]);

  if (direct) return direct;
  if (!remote) return null;
  return cached ?? (resolved?.source === remote ? resolved.dataUrl : null);
}
