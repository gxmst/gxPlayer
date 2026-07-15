type TauriInternals = {
  invoke?: unknown;
  metadata?: {
    currentWindow?: {
      label?: unknown;
    };
  };
};

type TauriGlobal = typeof globalThis & {
  isTauri?: unknown;
  __TAURI_INTERNALS__?: TauriInternals;
};

/**
 * `getCurrentWindow()` reads Tauri's injected globals synchronously, so a
 * rejected promise cannot protect browser-only development from that access.
 */
export function hasTauriWindowRuntime(): boolean {
  const runtime = globalThis as TauriGlobal;
  return runtime.isTauri === true
    && typeof runtime.__TAURI_INTERNALS__?.invoke === "function"
    && typeof runtime.__TAURI_INTERNALS__.metadata?.currentWindow?.label === "string";
}
