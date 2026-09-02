import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The theme surfaces form a ladder: shell -> content -> list container -> card.
 * Each rung has to be visibly above the one below it. Twice now the tokens have
 * drifted until adjacent layers composited to within 1.01 of each other, which
 * reads as one flat sheet and makes every shadow in the app invisible.
 *
 * Contrast is computed on the composited colour, not the token, because these are
 * translucent lifts over the theme base: the alpha alone says nothing about how
 * the result lands.
 */

const CSS = readFileSync(new URL("../App.css", import.meta.url), "utf8");

/** Below this, two large adjacent surfaces are indistinguishable in practice. */
const MIN_ADJACENT_CONTRAST = 1.02;

type Rgba = { r: number; g: number; b: number; a: number };

function declarationBlock(selector: string): string {
  const index = CSS.indexOf(selector);
  if (index < 0) throw new Error(`selector not found in App.css: ${selector}`);
  const start = CSS.indexOf("{", index);
  const end = CSS.indexOf("}", start);
  if (start < 0 || end < 0) throw new Error(`unterminated block for ${selector}`);
  return CSS.slice(start, end);
}

function token(block: string, name: string): string | null {
  const match = new RegExp(`${name}:\\s*([^;]+);`).exec(block);
  return match ? match[1].trim() : null;
}

function parseColour(value: string): Rgba {
  const hex = /^#([0-9a-f]{6})$/i.exec(value);
  if (hex) {
    const int = Number.parseInt(hex[1], 16);
    return { r: (int >> 16) & 255, g: (int >> 8) & 255, b: int & 255, a: 1 };
  }
  const rgba = /^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:[,/\s]+([\d.]+))?\s*\)$/i.exec(value);
  if (rgba) {
    return {
      r: Number(rgba[1]),
      g: Number(rgba[2]),
      b: Number(rgba[3]),
      a: rgba[4] === undefined ? 1 : Number(rgba[4]),
    };
  }
  throw new Error(`unsupported colour syntax: ${value}`);
}

/** Source-over compositing, matching what the compositor actually does. */
function composite(top: Rgba, bottom: Rgba): Rgba {
  const alpha = top.a + bottom.a * (1 - top.a);
  const channel = (t: number, b: number) =>
    (t * top.a + b * bottom.a * (1 - top.a)) / (alpha || 1);
  return {
    r: channel(top.r, bottom.r),
    g: channel(top.g, bottom.g),
    b: channel(top.b, bottom.b),
    a: alpha,
  };
}

function relativeLuminance({ r, g, b }: Rgba): number {
  const channel = (value: number) => {
    const c = value / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrastRatio(a: Rgba, b: Rgba): number {
  const [high, low] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
  return (high + 0.05) / (low + 0.05);
}

const THEMES: Array<{ label: string; selector: string }> = [
  { label: "dark", selector: ":root" },
  { label: "light", selector: '.app-shell[data-theme="light"]' },
  { label: "warm", selector: '.app-shell[data-theme="warm"]' },
  { label: "cool", selector: '.app-shell[data-theme="cool"]' },
];

const LADDER = ["--content-bg", "--list-bg", "--panel-bg"] as const;

describe("theme surface ladder", () => {
  const root = declarationBlock(":root");

  it.each(THEMES)("$label keeps every rung visibly above the last", ({ selector }) => {
    const block = selector === ":root" ? root : declarationBlock(selector);
    const read = (name: string) => {
      const value = token(block, name) ?? token(root, name);
      if (!value) throw new Error(`token ${name} missing from ${selector} and :root`);
      return value;
    };

    const base = parseColour(read("--base"));
    let below = { name: "--base", colour: base };

    for (const name of LADDER) {
      const colour = composite(parseColour(read(name)), base);
      const ratio = contrastRatio(colour, below.colour);
      expect(
        ratio,
        `${selector}: ${below.name} -> ${name} composited to ${ratio.toFixed(4)}, `
        + `below the ${MIN_ADJACENT_CONTRAST} floor — the two surfaces read as one`,
      ).toBeGreaterThanOrEqual(MIN_ADJACENT_CONTRAST);
      below = { name, colour };
    }
  });

  /**
   * Dark themes lift toward a tint, so every rung is brighter than the last.
   * Light is deliberately different: its base is already near-white, so the
   * content wash seats *below* the base and the raised surfaces climb back up
   * from there. Only the climb above content is a shared invariant.
   */
  it("raises list and card above the content wash in every theme", () => {
    for (const { selector } of THEMES) {
      const block = selector === ":root" ? root : declarationBlock(selector);
      const read = (name: string) => token(block, name) ?? token(root, name)!;
      const base = parseColour(read("--base"));
      const luminanceOf = (name: string) =>
        relativeLuminance(composite(parseColour(read(name)), base));

      const content = luminanceOf("--content-bg");
      const list = luminanceOf("--list-bg");
      const panel = luminanceOf("--panel-bg");

      expect(list, `${selector}: --list-bg sits below --content-bg`).toBeGreaterThan(content);
      expect(panel, `${selector}: --panel-bg sits below --list-bg`).toBeGreaterThan(list);

      if (relativeLuminance(base) < 0.5) {
        expect(content, `${selector}: dark themes lift the content wash above the base`)
          .toBeGreaterThan(relativeLuminance(base));
      } else {
        expect(content, `${selector}: light seats the content wash below its near-white base`)
          .toBeLessThan(relativeLuminance(base));
      }
    }
  });
});
