import assert from "node:assert/strict";
import test from "node:test";
import {
  frontendNextIndex,
  moveIndex,
  pickFailureSkipIndex,
} from "./playlistLogic.ts";

test("sequential ended advances then stops", () => {
  const played = new Set<number>();
  const rng = { state: 1 };
  assert.equal(frontendNextIndex("sequential", 0, 3, "ended", played, rng), 1);
  assert.equal(frontendNextIndex("sequential", 2, 3, "ended", played, rng), null);
});

test("repeat_one ended stays, next advances", () => {
  const played = new Set<number>();
  const rng = { state: 1 };
  assert.equal(frontendNextIndex("repeat_one", 1, 3, "ended", played, rng), 1);
  assert.equal(frontendNextIndex("repeat_one", 1, 3, "next", played, rng), 2);
});

test("failure skip never loops single-item repeat", () => {
  const tried = new Set([0]);
  const played = new Set<number>();
  const rng = { state: 42 };
  assert.equal(pickFailureSkipIndex("repeat_one", 0, 1, tried, played, rng), null);
  assert.equal(pickFailureSkipIndex("sequential", 0, 3, tried, played, rng), 1);
});

test("moveIndex reorders", () => {
  assert.deepEqual(moveIndex(["a", "b", "c"], 0, 2), ["b", "c", "a"]);
  assert.deepEqual(moveIndex(["a", "b", "c"], 2, 0), ["c", "a", "b"]);
});
