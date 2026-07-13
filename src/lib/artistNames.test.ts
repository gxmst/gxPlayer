import { describe, expect, it } from "vitest";
import { splitArtistNames } from "./artistNames";

describe("splitArtistNames", () => {
  it("splits common Chinese and collaboration separators", () => {
    expect(splitArtistNames("A 、B")).toEqual(["A", "B"]);
    expect(splitArtistNames("甲，乙 / 丙 & 丁 feat. 戊")).toEqual(["甲", "乙", "丙", "丁", "戊"]);
  });

  it("keeps compact slash names intact and removes duplicate credits", () => {
    expect(splitArtistNames("AC/DC")).toEqual(["AC/DC"]);
    expect(splitArtistNames("A、a、B")).toEqual(["A", "B"]);
    expect(splitArtistNames("  ")).toEqual([]);
  });
});
