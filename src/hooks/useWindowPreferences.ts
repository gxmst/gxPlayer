import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export function useWindowPreferences(onError: (error: unknown) => void) {
  const [alwaysOnTop, setAlwaysOnTop] = useState(false);
  const [miniMode, setMiniMode] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => window.innerWidth <= 1200);
  const miniModeRef = useRef(miniMode);
  const saveTimer = useRef<number | null>(null);
  const errorHandler = useRef(onError);
  miniModeRef.current = miniMode;
  errorHandler.current = onError;

  useEffect(() => {
    void invoke<{ alwaysOnTop?: boolean; miniMode?: boolean }>("window_get_state")
      .then((state) => {
        setAlwaysOnTop(Boolean(state.alwaysOnTop));
        setMiniMode(Boolean(state.miniMode));
      })
      .catch(() => undefined);

    const scheduleSave = () => {
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(() => {
        void invoke("window_save_state", { miniMode: miniModeRef.current }).catch(() => undefined);
      }, 400);
    };
    const resized = getCurrentWindow().onResized(scheduleSave);
    const moved = getCurrentWindow().onMoved(scheduleSave);
    return () => {
      void resized.then((dispose) => dispose());
      void moved.then((dispose) => dispose());
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
    };
  }, []);

  const toggleAlwaysOnTop = useCallback(async () => {
    const next = !alwaysOnTop;
    try {
      await invoke("window_set_always_on_top", { enabled: next });
      setAlwaysOnTop(next);
      return true;
    } catch (error) {
      errorHandler.current(error);
      return false;
    }
  }, [alwaysOnTop]);

  const toggleMiniMode = useCallback(async () => {
    const next = !miniMode;
    try {
      await invoke("window_set_mini_mode", { enabled: next });
      setMiniMode(next);
      return true;
    } catch (error) {
      errorHandler.current(error);
      return false;
    }
  }, [miniMode]);

  return {
    alwaysOnTop,
    miniMode,
    sidebarCollapsed,
    setSidebarCollapsed,
    toggleAlwaysOnTop,
    toggleMiniMode,
  };
}
