type TauriInternals = {
  invoke?: unknown;
};

type TauriGlobal = typeof globalThis & {
  isTauri?: unknown;
  __TAURI_INTERNALS__?: TauriInternals;
};

/**
 * Tauri's IPC function is the stable runtime boundary. Metadata is not
 * guaranteed to be present in every packaged WebView bootstrap, so requiring
 * a window label would incorrectly route desktop builds to browser mocks.
 */
export function hasTauriWindowRuntime(): boolean {
  const runtime = globalThis as TauriGlobal;
  return typeof runtime.__TAURI_INTERNALS__?.invoke === "function";
}
