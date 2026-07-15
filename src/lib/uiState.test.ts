import { describe, expect, it } from "vitest";
import { nextOptionIndex, putLruValue, shouldSkipAfterStart } from "./uiState";

describe("playback start outcomes", () => {
  it("skips only hard failures", () => {
    expect(shouldSkipAfterStart({ outcome: "failed", failureKind: "track_unavailable" })).toBe(true);
    expect(shouldSkipAfterStart({ outcome: "failed", failureKind: "network" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "failed", failureKind: "authentication" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "failed", failureKind: "rate_limited" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "failed" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "cancelled" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "stale" })).toBe(false);
    expect(shouldSkipAfterStart({ outcome: "started" })).toBe(false);
  });
});

describe("search keyboard selection", () => {
  it("wraps within the rendered option range", () => {
    expect(nextOptionIndex(-1, 4, 1)).toBe(0);
    expect(nextOptionIndex(-1, 4, -1)).toBe(3);
    expect(nextOptionIndex(3, 4, 1)).toBe(0);
    expect(nextOptionIndex(0, 4, -1)).toBe(3);
    expect(nextOptionIndex(0, 0, 1)).toBe(-1);
  });
});

describe("cover cache", () => {
  it("keeps the newest values and refreshes access order", () => {
    const full = putLruValue(putLruValue({ a: 1 }, "b", 2, 2), "c", 3, 2);
    expect(full).toEqual({ b: 2, c: 3 });
    const touched = putLruValue(full, "b", 2, 2);
    expect(Object.keys(touched)).toEqual(["c", "b"]);
  });
});
