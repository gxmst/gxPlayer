import { useEffect, useRef } from "react";

export function usePlayerShortcuts(actions: {
  onSearch: () => void; onToggle: () => void; onNext: () => void; onPrevious: () => void; onQueue: () => void;
}) {
  const current = useRef(actions);
  current.current = actions;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.repeat || document.querySelector('[aria-modal="true"]')) return;
      const modifier = event.ctrlKey || event.metaKey;
      if (modifier && event.key.toLowerCase() === "k") {
        event.preventDefault();
        current.current.onSearch();
        return;
      }
      const target = event.target;
      if (target instanceof Element && target.closest('input, textarea, select, button, [role="tab"], [contenteditable="true"]')) return;
      const action = event.code === "Space" && !modifier && !event.altKey ? current.current.onToggle
        : modifier && event.key === "ArrowRight" ? current.current.onNext
          : modifier && event.key === "ArrowLeft" ? current.current.onPrevious
            : modifier && event.key.toLowerCase() === "j" ? current.current.onQueue : null;
      if (action) { event.preventDefault(); action(); }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);
}
