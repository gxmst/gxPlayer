import { useEffect, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauriClient";
import { EMPTY_ENGINE, type EngineSnapshot } from "../types";

/**
 * Prefer pushed snapshots when the backend exposes them and keep a low-frequency
 * poll as compatibility fallback. This avoids the former 150 ms whole-app render.
 */
export function useEngineSnapshot(onError: (error: unknown) => void) {
  const [snapshot, setSnapshot] = useState<EngineSnapshot>(EMPTY_ENGINE);
  const errorHandler = useRef(onError);
  errorHandler.current = onError;

  useEffect(() => {
    let disposed = false;
    const lastPushAt = { value: 0 };
    const update = async () => {
      const startedAt = performance.now();
      try {
        const next = await invoke<EngineSnapshot>("player_snapshot");
        if (
          !disposed
          && lastPushAt.value <= startedAt
          && performance.now() - lastPushAt.value >= 500
        ) {
          setSnapshot(next);
        }
      } catch (error) {
        if (!disposed) errorHandler.current(error);
      }
    };

    void update();
    const timer = window.setInterval(update, 750);
    const unlisten = listen<EngineSnapshot>("gx-player-snapshot", (event) => {
      if (!disposed) {
        lastPushAt.value = performance.now();
        setSnapshot(event.payload);
      }
    });
    return () => {
      disposed = true;
      window.clearInterval(timer);
      void unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  return [snapshot, setSnapshot] as const;
}
