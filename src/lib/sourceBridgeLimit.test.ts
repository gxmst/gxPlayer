import { describe, expect, it } from "vitest";
import {
  MAX_SOURCE_BRIDGE_CALLS,
  SOURCE_BRIDGE_LIMIT_ERROR,
  hasSourceBridgeCapacity,
} from "./sourceBridgeLimit";

describe("source bridge concurrency limit", () => {
  it("accepts calls below the boundary and rejects the next call", () => {
    expect(hasSourceBridgeCapacity(0)).toBe(true);
    expect(hasSourceBridgeCapacity(MAX_SOURCE_BRIDGE_CALLS - 1)).toBe(true);
    expect(hasSourceBridgeCapacity(MAX_SOURCE_BRIDGE_CALLS)).toBe(false);
    expect(SOURCE_BRIDGE_LIMIT_ERROR).toContain(String(MAX_SOURCE_BRIDGE_CALLS));
  });

  it("rejects malformed counters", () => {
    expect(hasSourceBridgeCapacity(-1)).toBe(false);
    expect(hasSourceBridgeCapacity(Number.NaN)).toBe(false);
    expect(hasSourceBridgeCapacity(1.5)).toBe(false);
  });
});
