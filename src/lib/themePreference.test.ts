import { describe, expect, it } from "vitest";
import {
  DEFAULT_THEME,
  THEME_OPTIONS,
  THEME_STORAGE_KEY,
  loadThemePreference,
  parseTheme,
  saveThemePreference,
} from "./themePreference";

function storage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    value: (key: string) => values.get(key) ?? null,
  };
}

describe("theme preference", () => {
  it("accepts exactly the four supported themes", () => {
    expect(THEME_OPTIONS.map((option) => option.id)).toEqual(["dark", "light", "warm", "cool"]);
    expect(parseTheme("light")).toBe("light");
    expect(parseTheme("unknown")).toBe(DEFAULT_THEME);
    expect(parseTheme(null)).toBe(DEFAULT_THEME);
  });

  it("round-trips a selected theme through storage", () => {
    const saved = storage();
    saveThemePreference("cool", saved);
    expect(saved.value(THEME_STORAGE_KEY)).toBe("cool");
    expect(loadThemePreference(saved)).toBe("cool");
  });

  it("falls back safely when storage operations fail", () => {
    const broken = {
      getItem: () => { throw new Error("blocked"); },
      setItem: () => { throw new Error("quota"); },
    };
    expect(loadThemePreference(broken)).toBe(DEFAULT_THEME);
    expect(() => saveThemePreference("warm", broken)).not.toThrow();
  });
});
