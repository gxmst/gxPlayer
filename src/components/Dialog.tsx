import {
  useId,
  useLayoutEffect,
  useRef,
  type ReactNode,
  type RefObject,
} from "react";

export type DialogCloseReason = "escape" | "backdrop" | "close-button";
export type DialogSize = "small" | "medium" | "large";

export type DialogProps = {
  open: boolean;
  title: ReactNode;
  eyebrow?: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  size?: DialogSize;
  role?: "dialog" | "alertdialog";
  busy?: boolean;
  showClose?: boolean;
  closeOnBackdrop?: boolean;
  initialFocusRef?: RefObject<HTMLElement | null>;
  onRequestClose: (reason: DialogCloseReason) => void;
  className?: string;
};

type DialogStackEntry = {
  id: symbol;
  backdrop: () => HTMLElement | null;
  surface: () => HTMLElement | null;
  busy: () => boolean;
  close: (reason: DialogCloseReason) => void;
};

const dialogStack: DialogStackEntry[] = [];
let listeningForKeys = false;

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "iframe",
  "object",
  "embed",
  "[contenteditable='true']",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function isTopDialog(entry: DialogStackEntry): boolean {
  return dialogStack[dialogStack.length - 1] === entry;
}

function isFocusable(element: HTMLElement): boolean {
  if (element.tabIndex < 0 || element.hidden || element.getAttribute("aria-hidden") === "true") return false;
  if (element.closest("[hidden], [inert], [aria-hidden='true']")) return false;
  return !("disabled" in element && Boolean((element as HTMLButtonElement).disabled));
}

function focusableElements(surface: HTMLElement): HTMLElement[] {
  return Array.from(surface.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(isFocusable);
}

function focusWithoutScroll(element: HTMLElement): void {
  try {
    element.focus({ preventScroll: true });
  } catch {
    element.focus();
  }
}

function focusDialog(entry: DialogStackEntry, preferred?: HTMLElement | null): void {
  const surface = entry.surface();
  if (!surface?.isConnected) return;
  if (preferred?.isConnected && surface.contains(preferred) && !preferred.hidden) {
    focusWithoutScroll(preferred);
    if (document.activeElement === preferred) return;
  }
  focusWithoutScroll(focusableElements(surface)[0] ?? surface);
}

function focusSafeFallback(): void {
  const topDialog = dialogStack[dialogStack.length - 1];
  if (topDialog) {
    focusDialog(topDialog);
    return;
  }

  const body = document.body;
  const previousTabIndex = body.getAttribute("tabindex");
  body.setAttribute("tabindex", "-1");
  focusWithoutScroll(body);
  if (previousTabIndex === null) body.removeAttribute("tabindex");
  else body.setAttribute("tabindex", previousTabIndex);
}

function trapTab(event: KeyboardEvent, entry: DialogStackEntry): void {
  const surface = entry.surface();
  if (!surface) return;

  const focusable = focusableElements(surface);
  event.preventDefault();
  if (focusable.length === 0) {
    focusWithoutScroll(surface);
    return;
  }

  const activeElement = document.activeElement;
  const activeIndex = activeElement instanceof HTMLElement ? focusable.indexOf(activeElement) : -1;
  if (activeIndex < 0) {
    focusWithoutScroll(event.shiftKey ? focusable[focusable.length - 1] : focusable[0]);
    return;
  }

  const offset = event.shiftKey ? -1 : 1;
  const nextIndex = (activeIndex + offset + focusable.length) % focusable.length;
  focusWithoutScroll(focusable[nextIndex]);
}

function handleDocumentKeyDown(event: KeyboardEvent): void {
  const entry = dialogStack[dialogStack.length - 1];
  if (!entry) return;

  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    if (!entry.busy()) entry.close("escape");
    return;
  }

  if (event.key === "Tab") {
    event.stopPropagation();
    trapTab(event, entry);
  }
}

function updateKeyListener(): void {
  if (typeof document === "undefined") return;
  const shouldListen = dialogStack.length > 0;
  if (shouldListen === listeningForKeys) return;
  listeningForKeys = shouldListen;
  if (shouldListen) document.addEventListener("keydown", handleDocumentKeyDown, true);
  else document.removeEventListener("keydown", handleDocumentKeyDown, true);
}

function syncDialogLayers(): void {
  dialogStack.forEach((entry, index) => {
    const backdrop = entry.backdrop();
    if (!backdrop) return;
    const top = index === dialogStack.length - 1;
    backdrop.style.zIndex = String(220 + index * 2);
    backdrop.style.pointerEvents = top ? "" : "none";
    backdrop.toggleAttribute("inert", !top);
    if (top) backdrop.removeAttribute("aria-hidden");
    else backdrop.setAttribute("aria-hidden", "true");
  });
}

function pushDialog(entry: DialogStackEntry): void {
  const existingIndex = dialogStack.findIndex((candidate) => candidate.id === entry.id);
  if (existingIndex >= 0) dialogStack.splice(existingIndex, 1);
  dialogStack.push(entry);
  syncDialogLayers();
  updateKeyListener();
}

function removeDialog(entry: DialogStackEntry): boolean {
  const wasTop = isTopDialog(entry);
  const index = dialogStack.indexOf(entry);
  if (index >= 0) dialogStack.splice(index, 1);
  syncDialogLayers();
  updateKeyListener();
  if (wasTop) {
    const nextTop = dialogStack[dialogStack.length - 1];
    if (nextTop) focusDialog(nextTop);
  }
  return wasTop;
}

export function Dialog({
  open,
  title,
  eyebrow,
  description,
  children,
  actions,
  size = "medium",
  role = "dialog",
  busy = false,
  showClose = true,
  closeOnBackdrop = true,
  initialFocusRef,
  onRequestClose,
  className,
}: DialogProps) {
  const backdropRef = useRef<HTMLDivElement>(null);
  const surfaceRef = useRef<HTMLElement>(null);
  const instanceId = useRef(Symbol("dialog"));
  const busyRef = useRef(busy);
  const closeRef = useRef(onRequestClose);
  const preferredFocusRef = useRef(initialFocusRef);
  const titleId = useId();
  const descriptionId = useId();

  busyRef.current = busy;
  closeRef.current = onRequestClose;
  preferredFocusRef.current = initialFocusRef;

  useLayoutEffect(() => {
    if (!open || typeof document === "undefined") return;

    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const entry: DialogStackEntry = {
      id: instanceId.current,
      backdrop: () => backdropRef.current,
      surface: () => surfaceRef.current,
      busy: () => busyRef.current,
      close: (reason) => closeRef.current(reason),
    };
    pushDialog(entry);

    queueMicrotask(() => {
      if (isTopDialog(entry)) focusDialog(entry, preferredFocusRef.current?.current);
    });

    return () => {
      const wasTop = removeDialog(entry);
      if (!wasTop) return;
      queueMicrotask(() => {
        if (dialogStack.some((candidate) => candidate.id === entry.id)) return;
        if (opener?.isConnected) {
          focusWithoutScroll(opener);
          if (document.activeElement === opener) return;
        }
        focusSafeFallback();
      });
    };
  }, [open]);

  if (!open) return null;

  const requestClose = (reason: DialogCloseReason) => {
    const entry = dialogStack.find((candidate) => candidate.id === instanceId.current);
    if (!busy && entry && isTopDialog(entry)) {
      onRequestClose(reason);
    }
  };

  return (
    <div
      ref={backdropRef}
      className={`modal-backdrop app-dialog-backdrop${busy ? " app-dialog-backdrop--busy" : ""}`}
      role="presentation"
      onMouseDown={(event) => {
        if (closeOnBackdrop && event.target === event.currentTarget) requestClose("backdrop");
      }}
    >
      <section
        ref={surfaceRef}
        className={`config-modal app-dialog app-dialog--${size}${className ? ` ${className}` : ""}`}
        role={role}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description !== undefined && description !== null ? descriptionId : undefined}
        aria-busy={busy || undefined}
        tabIndex={-1}
        data-dialog-surface=""
      >
        <header className="app-dialog__header">
          <div className="app-dialog__heading">
            {eyebrow !== undefined && eyebrow !== null && <p className="eyebrow">{eyebrow}</p>}
            <h2 id={titleId}>{title}</h2>
            {description !== undefined && description !== null && <p id={descriptionId}>{description}</p>}
          </div>
          {showClose && (
            <button
              type="button"
              className="icon-button app-dialog__close"
              aria-label="关闭对话框"
              disabled={busy}
              onClick={() => requestClose("close-button")}
            >
              ×
            </button>
          )}
        </header>
        {children !== undefined && children !== null && <div className="app-dialog__body">{children}</div>}
        {actions !== undefined && actions !== null && (
          <footer className="modal-actions app-dialog__actions">{actions}</footer>
        )}
      </section>
    </div>
  );
}
