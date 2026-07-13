import { describe, expect, it } from "vitest";
import { windowCanAnimate } from "./useWindowActivity";

const activeWindow = {
  documentVisible: true,
  focused: true,
  minimized: false,
  visible: true,
};

describe("windowCanAnimate", () => {
  it("allows animation only for a visible focused window", () => {
    expect(windowCanAnimate(activeWindow)).toBe(true);
    expect(windowCanAnimate({ ...activeWindow, documentVisible: false })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, focused: false })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, minimized: true })).toBe(false);
    expect(windowCanAnimate({ ...activeWindow, visible: false })).toBe(false);
  });
});
