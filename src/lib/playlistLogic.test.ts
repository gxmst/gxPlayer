import { describe, expect, it } from "vitest";
import {
  frontendNextIndex,
  moveIndex,
  pickFailureSkipIndex,
} from "./playlistLogic.ts";

describe("playlist advancement", () => {
it("sequential ended advances then stops", () => {
  const played = new Set<number>();
  const rng = { state: 1 };
  expect(frontendNextIndex("sequential", 0, 3, "ended", played, rng)).toBe(1);
  expect(frontendNextIndex("sequential", 2, 3, "ended", played, rng)).toBeNull();
});

it("repeat_one ended stays, next advances", () => {
  const played = new Set<number>();
  const rng = { state: 1 };
  expect(frontendNextIndex("repeat_one", 1, 3, "ended", played, rng)).toBe(1);
  expect(frontendNextIndex("repeat_one", 1, 3, "next", played, rng)).toBe(2);
});

it("failure skip never loops single-item repeat", () => {
  const tried = new Set([0]);
  const played = new Set<number>();
  const rng = { state: 42 };
  expect(pickFailureSkipIndex("repeat_one", 0, 1, tried, played, rng)).toBeNull();
  expect(pickFailureSkipIndex("sequential", 0, 3, tried, played, rng)).toBe(1);
});

it("moveIndex reorders", () => {
  expect(moveIndex(["a", "b", "c"], 0, 2)).toEqual(["b", "c", "a"]);
  expect(moveIndex(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
});
});
