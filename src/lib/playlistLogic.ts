/** Pure playlist advancement helpers — shared by UI and unit tests. */

export type PlayMode = "sequential" | "repeat_all" | "repeat_one" | "shuffle";

export function frontendNextIndex(
  mode: PlayMode,
  current: number,
  length: number,
  intent: "ended" | "next" | "previous",
  shufflePlayed: Set<number>,
  rng: { state: number },
): number | null {
  if (length <= 0) return null;
  if (intent === "previous") {
    if (mode === "shuffle") {
      shufflePlayed.add(current);
      return pickShuffleIndex(length, shufflePlayed, current, rng);
    }
    if (mode === "repeat_all") return current === 0 ? length - 1 : current - 1;
    return current > 0 ? current - 1 : 0;
  }
  if (mode === "repeat_one" && intent === "ended") return current;
  if (mode === "shuffle") {
    shufflePlayed.add(current);
    return pickShuffleIndex(length, shufflePlayed, current, rng);
  }
  if (mode === "repeat_all") {
    if (length === 1) return 0;
    return (current + 1) % length;
  }
  const next = current + 1;
  return next < length ? next : null;
}

export function lcgNext(rng: { state: number }): number {
  rng.state = Math.imul(rng.state, 1664525) + 1013904223;
  return rng.state >>> 0;
}

export function pickShuffleIndex(
  length: number,
  played: Set<number>,
  preferNot: number,
  rng: { state: number },
): number {
  let available = Array.from({ length }, (_, i) => i).filter((i) => !played.has(i));
  if (available.length === 0) {
    played.clear();
    available = Array.from({ length }, (_, i) => i);
    if (length > 1) available = available.filter((i) => i !== preferNot);
  }
  if (available.length === 0) return 0;
  const choice = available[lcgNext(rng) % available.length]!;
  played.add(choice);
  return choice;
}

export function pickFailureSkipIndex(
  mode: PlayMode,
  current: number,
  length: number,
  tried: Set<number>,
  shufflePlayed: Set<number>,
  rng: { state: number },
): number | null {
  if (length <= 0) return null;
  const untried = Array.from({ length }, (_, i) => i).filter((i) => !tried.has(i));
  if (untried.length === 0) return null;

  if (mode === "shuffle") {
    const unplayedUntried = untried.filter((i) => !shufflePlayed.has(i));
    const pool = unplayedUntried.length > 0 ? unplayedUntried : untried;
    const choice = pool[lcgNext(rng) % pool.length]!;
    shufflePlayed.add(choice);
    return choice;
  }

  if (mode === "sequential") {
    const after = untried.filter((i) => i > current).sort((a, b) => a - b);
    return after[0] ?? null;
  }

  for (let step = 1; step <= length; step += 1) {
    const candidate = (current + step) % length;
    if (!tried.has(candidate)) return candidate;
  }
  return null;
}

/** Reorder list by moving `from` index to `to` index. */
export function moveIndex<T>(list: T[], from: number, to: number): T[] {
  if (from < 0 || to < 0 || from >= list.length || to >= list.length || from === to) {
    return list.slice();
  }
  const next = list.slice();
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item!);
  return next;
}
