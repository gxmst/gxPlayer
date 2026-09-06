import { useEffect, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauriClient";
import { EMPTY_ENGINE, type EngineSnapshot } from "../types";

// IPC creates fresh objects even when the queue or DSP controls did not change.
// Keep those references stable so their derived views can remain memoized.
function shareValue(current: unknown, next: unknown): unknown {
  if (Object.is(current, next)) return current;
  if (!current || !next || typeof current !== "object" || typeof next !== "object"
    || Array.isArray(current) !== Array.isArray(next)) return next;
  const previous = current as Record<string, unknown>;
  const incoming = next as Record<string, unknown>;
  const keys = Object.keys(incoming);
  let unchanged = keys.length === Object.keys(previous).length;
  const result: Record<string, unknown> = Array.isArray(next) ? [] as unknown as Record<string, unknown> : {};
  for (const key of keys) {
    result[key] = shareValue(previous[key], incoming[key]);
    unchanged &&= Object.prototype.hasOwnProperty.call(previous, key) && result[key] === previous[key];
  }
  return unchanged ? current : result;
}

/**
 * Prefer pushed snapshots when the backend exposes them and keep a low-frequency
 * poll as compatibility fallback. This avoids the former 150 ms whole-app render.
 */
export function useEngineSnapshot(
  onError: (error: unknown) => void,
  mergeIncoming?: (incoming: EngineSnapshot, current: EngineSnapshot) => EngineSnapshot,
) {
  const [snapshot, setSnapshot] = useState<EngineSnapshot>(EMPTY_ENGINE);
  const errorHandler = useRef(onError);
  errorHandler.current = onError;
  const mergeRef = useRef(mergeIncoming);
  mergeRef.current = mergeIncoming;

  useEffect(() => {
    let disposed = false;
    let lastPushAt: number | null = null;
    let pushRevision = 0;
    let polling = false;
    const applySnapshot = (next: EngineSnapshot) => setSnapshot((current) => {
      const merged = mergeRef.current ? mergeRef.current(next, current) : next;
      return shareValue(current, merged) as EngineSnapshot;
    });
    const update = async () => {
      if (polling || (lastPushAt !== null && performance.now() - lastPushAt < 750)) return;
      polling = true;
      const revision = pushRevision;
      try {
        const next = await invoke<EngineSnapshot>("player_snapshot");
        if (!disposed && revision === pushRevision) applySnapshot(next);
      } catch (error) {
        if (!disposed) errorHandler.current(error);
      } finally {
        polling = false;
      }
    };

    void update();
    const timer = window.setInterval(update, 750);
    const unlisten = listen<EngineSnapshot>("gx-player-snapshot", (event) => {
      if (!disposed) {
        lastPushAt = performance.now();
        pushRevision += 1;
        applySnapshot(event.payload);
      }
    }).catch((error) => {
      if (!disposed) errorHandler.current(error);
      return () => undefined;
    });
    return () => {
      disposed = true;
      window.clearInterval(timer);
      void unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  return [snapshot, setSnapshot] as const;
}
