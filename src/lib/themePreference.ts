export const THEME_STORAGE_KEY = "gxplayer.theme";

export const THEME_OPTIONS = [
  { id: "dark", label: "深色", description: "默认深色" },
  { id: "light", label: "亮色", description: "清爽亮色" },
  { id: "warm", label: "暖夜", description: "暖调深色" },
  { id: "cool", label: "冷夜", description: "冷调深色" },
] as const;

export type ThemeId = (typeof THEME_OPTIONS)[number]["id"];
export type ThemeStorage = Pick<Storage, "getItem" | "setItem">;

export const DEFAULT_THEME: ThemeId = "dark";

export function isThemeId(value: unknown): value is ThemeId {
  return typeof value === "string" && THEME_OPTIONS.some((option) => option.id === value);
}

export function parseTheme(value: string | null | undefined): ThemeId {
  return isThemeId(value) ? value : DEFAULT_THEME;
}

function defaultStorage(): ThemeStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function loadThemePreference(storage: ThemeStorage | null = defaultStorage()): ThemeId {
  if (!storage) return DEFAULT_THEME;
  try {
    return parseTheme(storage.getItem(THEME_STORAGE_KEY));
  } catch {
    return DEFAULT_THEME;
  }
}

export function saveThemePreference(theme: ThemeId, storage: ThemeStorage | null = defaultStorage()): void {
  if (!storage) return;
  try {
    storage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Preferences are best effort when storage is unavailable or full.
  }
}
