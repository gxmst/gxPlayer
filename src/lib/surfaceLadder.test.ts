import { readFileSync } from "node:fs";
import { parse } from "postcss";
import { describe, expect, it } from "vitest";

const CSS = parse(readFileSync(new URL("../styles/tokens.css", import.meta.url), "utf8"));

function declarationBlock(selector: string): Map<string, string> {
  const values = new Map<string, string>();
  CSS.walkRules(selector, (rule) => { rule.walkDecls((declaration) => { values.set(declaration.prop, declaration.value); }); });
  if (!values.size) throw new Error(`selector not found in tokens.css: ${selector}`);
  return values;
}

function readToken(block: Map<string, string>, root: Map<string, string>, name: string): string {
  let value = block.get(name) ?? root.get(name);
  if (!value) throw new Error(`token ${name} missing`);
  for (let i = 0; i < 8; i += 1) {
    const reference = /^var\((--[\w-]+)\)$/.exec(value);
    if (!reference) return value;
    value = block.get(reference[1]) ?? root.get(reference[1]) ?? value;
  }
  throw new Error(`unresolved token reference: ${name}`);
}

function parseHex(value: string): [number, number, number] {
  const match = /^#([0-9a-f]{6})$/i.exec(value);
  if (!match) throw new Error(`unsupported colour syntax: ${value}`);
  const int = Number.parseInt(match[1], 16);
  return [(int >> 16) & 255, (int >> 8) & 255, int & 255];
}

function luminance(value: string): number {
  const [r, g, b] = parseHex(value).map((channel) => {
    const normalized = channel / 255;
    return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (high + 0.05) / (low + 0.05);
}

const THEMES = [
  { label: "dark", selector: ":root" },
  { label: "light", selector: '.app-shell[data-theme="light"]' },
  { label: "warm", selector: '.app-shell[data-theme="warm"]' },
  { label: "cool", selector: '.app-shell[data-theme="cool"]' },
] as const;

describe("theme tokens", () => {
  const root = declarationBlock(":root");

  it.each(THEMES)("$label defines the shared surface and text contract", ({ selector }) => {
    const block = selector === ":root" ? root : declarationBlock(selector);
    for (const name of ["--base", "--content-bg", "--panel-bg", "--text-hi", "--text-mute", "--text-dim", "--accent"]) {
      expect(readToken(block, root, name), `${selector}: ${name}`).toMatch(/^(#|color-mix|transparent)/);
    }
  });

  it("keeps small text readable on the main surfaces of every theme", () => {
    for (const { selector } of THEMES) {
      const block = selector === ":root" ? root : declarationBlock(selector);
      for (const surface of ["--base", "--panel-bg", "--panel-strong", "--sidebar-bg", "--field-bg"]) {
        for (const text of ["--text-hi", "--text-mute", "--text-dim"]) {
          expect(contrast(readToken(block, root, text), readToken(block, root, surface)), `${selector}: ${text} on ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
      }
    }
  });

  it("keeps raised panels perceptibly separate from the base", () => {
    for (const { selector } of THEMES) {
      const block = selector === ":root" ? root : declarationBlock(selector);
      const base = readToken(block, root, "--base");
      const panel = readToken(block, root, "--panel-bg");
      expect(contrast(panel, base), `${selector}: panel against base`).toBeGreaterThanOrEqual(1.02);
    }
  });
});
