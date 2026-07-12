import { describe, expect, it } from "vitest";
import { deriveTransportCapabilities, isTransportAction } from "./transport";

describe("transport capabilities", () => {
  it("matches sequential and repeat-one boundaries", () => {
    expect(deriveTransportCapabilities({
      queueLength: 3,
      currentIndex: 2,
      hasCurrent: true,
      playMode: "sequential",
    })).toEqual({ hasCurrent: true, canPrevious: true, canNext: false });
    expect(deriveTransportCapabilities({
      queueLength: 3,
      currentIndex: 1,
      hasCurrent: true,
      playMode: "repeat_one",
    })).toEqual({ hasCurrent: true, canPrevious: true, canNext: true });
  });

  it("keeps wrapping and shuffle controls available", () => {
    for (const playMode of ["repeat_all", "shuffle"] as const) {
      expect(deriveTransportCapabilities({
        queueLength: 1,
        currentIndex: 0,
        hasCurrent: true,
        playMode,
      })).toEqual({ hasCurrent: true, canPrevious: true, canNext: true });
    }
  });

  it("does not infer queue navigation from an engine-only current item", () => {
    expect(deriveTransportCapabilities({
      queueLength: 0,
      currentIndex: null,
      hasCurrent: true,
      playMode: "sequential",
    })).toEqual({ hasCurrent: true, canPrevious: false, canNext: false });
  });

  it("accepts only the supported wire actions", () => {
    expect(isTransportAction("toggle")).toBe(true);
    expect(isTransportAction("stop")).toBe(false);
  });
});
