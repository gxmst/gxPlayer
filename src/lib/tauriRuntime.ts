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
  // The packaged bundle is always hosted by Tauri. Keeping this production
  // guard avoids a first-paint race where WebView2 has not exposed its IPC
  // globals yet and the app incorrectly switches to browser demo data.
  if (import.meta.env.PROD) return true;
  const tauriHost = runtime.location?.hostname?.toLowerCase() === "tauri.localhost";
  const tauriProtocol = runtime.location?.protocol === "tauri:";
  return tauriHost
    || tauriProtocol
    || typeof runtime.__TAURI_INTERNALS__?.invoke === "function";
}
