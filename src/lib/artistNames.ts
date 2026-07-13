const ARTIST_SEPARATOR = /(?:\s*[、，,；;＆&／]\s*|\s+\/\s+|\s+(?:feat(?:uring)?|ft)\.?\s+)/iu;

/** Split display-only artist credits without mutating the original track metadata. */
export function splitArtistNames(value: string): string[] {
  const normalized = value.trim();
  if (!normalized) return [];

  const seen = new Set<string>();
  return normalized
    .split(ARTIST_SEPARATOR)
    .map((name) => name.trim())
    .filter((name) => {
      if (!name) return false;
      const key = name.toLocaleLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
}
