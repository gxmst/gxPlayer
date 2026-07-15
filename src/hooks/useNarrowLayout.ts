import { useEffect, useState } from "react";

export const NARROW_LAYOUT_QUERY = "(max-width: 900.98px)";
const NARROW_LAYOUT_FALLBACK_MAX_WIDTH = 900.98;

function currentMatch(): boolean {
  if (typeof window === "undefined") return false;
  if (typeof window.matchMedia === "function") {
    return window.matchMedia(NARROW_LAYOUT_QUERY).matches;
  }
  return window.innerWidth <= NARROW_LAYOUT_FALLBACK_MAX_WIDTH;
}

export function useNarrowLayout(): boolean {
  const [matches, setMatches] = useState(currentMatch);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") {
      const update = () => setMatches(currentMatch());
      window.addEventListener("resize", update);
      update();
      return () => window.removeEventListener("resize", update);
    }

    const media = window.matchMedia(NARROW_LAYOUT_QUERY);
    const update = (event: MediaQueryListEvent) => setMatches(event.matches);
    setMatches(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  return matches;
}
