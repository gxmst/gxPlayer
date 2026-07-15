import { useEffect, useState } from "react";
import { getCurrentWindow } from "../lib/tauriClient";
import { hasTauriWindowRuntime } from "../lib/tauriRuntime";

type WindowActivity = {
  documentVisible: boolean;
  focused: boolean;
  minimized: boolean;
  visible: boolean;
};

export function windowCanAnimate(activity: WindowActivity): boolean {
  return activity.documentVisible && activity.focused && !activity.minimized && activity.visible;
}

export function useWindowActivity(): boolean {
  const [active, setActive] = useState(() => !document.hidden && document.hasFocus());

  useEffect(() => {
    if (!hasTauriWindowRuntime()) {
      const syncBrowserState = () => setActive(!document.hidden && document.hasFocus());
      const markUnfocused = () => setActive(false);

      document.addEventListener("visibilitychange", syncBrowserState);
      window.addEventListener("blur", markUnfocused);
      window.addEventListener("focus", syncBrowserState);
      return () => {
        document.removeEventListener("visibilitychange", syncBrowserState);
        window.removeEventListener("blur", markUnfocused);
        window.removeEventListener("focus", syncBrowserState);
      };
    }

    const appWindow = getCurrentWindow();
    let disposed = false;
    let syncRevision = 0;
    let activity: WindowActivity = {
      documentVisible: !document.hidden,
      focused: document.hasFocus(),
      minimized: false,
      visible: true,
    };

    const publish = () => {
      if (!disposed) setActive(windowCanAnimate(activity));
    };
    const syncNativeState = async () => {
      const revision = ++syncRevision;
      const [focused, minimized, visible] = await Promise.all([
        appWindow.isFocused().catch(() => document.hasFocus()),
        appWindow.isMinimized().catch(() => false),
        appWindow.isVisible().catch(() => true),
      ]);
      if (disposed || revision !== syncRevision) return;
      activity = {
        documentVisible: !document.hidden,
        focused,
        minimized,
        visible,
      };
      publish();
    };
    const markUnfocused = () => {
      syncRevision += 1;
      activity = { ...activity, focused: false, documentVisible: !document.hidden };
      publish();
    };
    const handleVisibilityChange = () => {
      if (document.hidden) {
        syncRevision += 1;
        activity = { ...activity, documentVisible: false };
        publish();
      } else {
        void syncNativeState();
      }
    };

    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("blur", markUnfocused);
    window.addEventListener("focus", syncNativeState);
    const focusChanged = appWindow.onFocusChanged(({ payload: focused }) => {
      if (focused) void syncNativeState();
      else markUnfocused();
    });
    const resized = appWindow.onResized(() => void syncNativeState());
    void syncNativeState();

    return () => {
      disposed = true;
      syncRevision += 1;
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("blur", markUnfocused);
      window.removeEventListener("focus", syncNativeState);
      void focusChanged.then((unlisten) => unlisten()).catch(() => undefined);
      void resized.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, []);

  return active;
}
