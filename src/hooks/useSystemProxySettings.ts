import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ProxyMode, ProxyStatus } from "../types";

export function useSystemProxySettings(onError: (error: unknown) => void) {
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const errorHandler = useRef(onError);
  errorHandler.current = onError;

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<ProxyStatus>("network_proxy_status");
      setStatus(next);
      return next;
    } catch (error) {
      errorHandler.current(error);
      return null;
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const setMode = useCallback(async (mode: ProxyMode) => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    try {
      const next = await invoke<ProxyStatus>("network_set_proxy_mode", { mode });
      setStatus(next);
      return true;
    } catch (error) {
      errorHandler.current(error);
      return false;
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }, []);

  return { status, busy, refresh, setMode };
}
