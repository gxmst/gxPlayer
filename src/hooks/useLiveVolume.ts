import { useCallback, useEffect, useRef, useState } from "react";

const TRAILING_FLUSH_MS = 180;

export function useLiveVolume(
  actualVolume: number,
  previewActualVolume: (volume: number) => Promise<unknown>,
  commitActualVolume: (volume: number) => Promise<unknown>,
  onError: (error: unknown) => void,
) {
  const [draftVolume, setDraftVolume] = useState<number | null>(null);
  const [isAdjustingVolume, setIsAdjustingVolume] = useState(false);
  const draftRef = useRef<number | null>(null);
  const actualVolumeRef = useRef(actualVolume);
  const sawDifferentActualRef = useRef(false);
  const pendingRef = useRef<{ volume: number; commit: boolean } | null>(null);
  const frameRef = useRef<number | null>(null);
  const trailingFlushRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);
  const disposedRef = useRef(false);
  const previewActualVolumeRef = useRef(previewActualVolume);
  const commitActualVolumeRef = useRef(commitActualVolume);
  const errorHandlerRef = useRef(onError);
  actualVolumeRef.current = actualVolume;
  previewActualVolumeRef.current = previewActualVolume;
  commitActualVolumeRef.current = commitActualVolume;
  errorHandlerRef.current = onError;

  const updateDraft = useCallback((volume: number | null) => {
    draftRef.current = volume;
    sawDifferentActualRef.current = volume !== null
      && Math.abs(actualVolumeRef.current - volume) >= 0.005;
    setDraftVolume(volume);
  }, []);

  const drainPending = useCallback(async (): Promise<void> => {
    if (disposedRef.current || inFlightRef.current) return;
    inFlightRef.current = true;
    try {
      while (!disposedRef.current && pendingRef.current !== null) {
        const pending = pendingRef.current;
        pendingRef.current = null;
        try {
          await (pending.commit ? commitActualVolumeRef.current : previewActualVolumeRef.current)(pending.volume);
        } catch (error) {
          if (disposedRef.current) return;
          if (pendingRef.current === null && draftRef.current === pending.volume) {
            updateDraft(null);
            setIsAdjustingVolume(false);
          }
          errorHandlerRef.current(error);
          return;
        }
      }
    } finally {
      inFlightRef.current = false;
      if (!disposedRef.current && pendingRef.current !== null) void drainPending();
    }
  }, [updateDraft]);

  const cancelScheduledFlush = useCallback(() => {
    if (frameRef.current !== null) {
      window.cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
    if (trailingFlushRef.current !== null) {
      window.clearTimeout(trailingFlushRef.current);
      trailingFlushRef.current = null;
    }
  }, []);

  const commitVolume = useCallback((volume: number) => {
    updateDraft(volume);
    setIsAdjustingVolume(false);
    pendingRef.current = { volume, commit: true };
    cancelScheduledFlush();
    void drainPending();
  }, [cancelScheduledFlush, drainPending, updateDraft]);

  const previewVolume = useCallback((volume: number) => {
    updateDraft(volume);
    setIsAdjustingVolume(true);
    pendingRef.current = { volume, commit: false };

    if (frameRef.current === null) {
      frameRef.current = window.requestAnimationFrame(() => {
        frameRef.current = null;
        void drainPending();
      });
    }

    if (trailingFlushRef.current !== null) window.clearTimeout(trailingFlushRef.current);
    trailingFlushRef.current = window.setTimeout(() => {
      trailingFlushRef.current = null;
      commitVolume(draftRef.current ?? volume);
    }, TRAILING_FLUSH_MS);
  }, [commitVolume, drainPending, updateDraft]);

  useEffect(() => {
    if (draftVolume === null) return;
    if (Math.abs(actualVolume - draftVolume) >= 0.005) {
      sawDifferentActualRef.current = true;
    } else if (sawDifferentActualRef.current) {
      updateDraft(null);
    }
  }, [actualVolume, draftVolume, updateDraft]);

  useEffect(() => {
    disposedRef.current = false;
    return () => {
      disposedRef.current = true;
      pendingRef.current = null;
      cancelScheduledFlush();
    };
  }, [cancelScheduledFlush]);

  return {
    shownVolume: draftVolume ?? actualVolume,
    isAdjustingVolume,
    previewVolume,
    commitVolume,
  };
}
