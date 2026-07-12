import type { PlayMode } from "../types";

export type TransportAction = "play" | "pause" | "toggle" | "next" | "previous";

export type TransportCapabilities = {
  revision: number;
  hasCurrent: boolean;
  canPrevious: boolean;
  canNext: boolean;
};

export function isTransportAction(value: string): value is TransportAction {
  return value === "play"
    || value === "pause"
    || value === "toggle"
    || value === "next"
    || value === "previous";
}

export function deriveTransportCapabilities(input: {
  queueLength: number;
  currentIndex: number | null;
  hasCurrent: boolean;
  playMode: PlayMode;
}): Omit<TransportCapabilities, "revision"> {
  const { queueLength, currentIndex, hasCurrent, playMode } = input;
  const hasQueueIndex = currentIndex !== null
    && Number.isInteger(currentIndex)
    && currentIndex >= 0
    && currentIndex < queueLength;
  if (!hasQueueIndex) {
    return {
      hasCurrent,
      canPrevious: false,
      canNext: false,
    };
  }

  return {
    hasCurrent: true,
    // Sequential/repeat-one Previous at index zero restarts the current track.
    canPrevious: true,
    canNext: playMode === "repeat_all"
      || playMode === "shuffle"
      || currentIndex + 1 < queueLength,
  };
}
