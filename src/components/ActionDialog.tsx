import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Dialog, type DialogCloseReason } from "./Dialog";
import "./ActionDialog.css";

export type ActionErrorKind =
  | "validation"
  | "cancelled"
  | "transient"
  | "permanent"
  | "postCommit";

export type ActionErrorClassification = {
  kind: ActionErrorKind;
  message: string;
};

export type ActionErrorContext = {
  phase: "run" | "afterSuccess" | "undo" | "afterUndo";
};

export type ActionErrorClassifier = (
  error: unknown,
  context: ActionErrorContext,
) => ActionErrorClassification;

export type ActionUndoSpec<TResult> = {
  label?: string;
  busyLabel?: string;
  run: (result: TResult) => void | Promise<void>;
  afterSuccess?: (result: TResult) => void | Promise<void>;
  classifyError?: ActionErrorClassifier;
  retrySafe?: boolean;
};

export type ActionSpec<TResult = void> = {
  title: ReactNode;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  busyLabel?: string;
  completedDescription?: ReactNode;
  tone?: "default" | "danger";
  role?: "dialog" | "alertdialog";
  run: () => TResult | Promise<TResult>;
  afterSuccess?: (result: TResult) => void | Promise<void>;
  classifyError?: ActionErrorClassifier;
  retrySafe?: boolean;
  undo?: ActionUndoSpec<TResult>;
};

export type ActionDialogStatus =
  | "ready"
  | "running"
  | "error"
  | "succeeded"
  | "undoing"
  | "undoError";

type StoredActionSpec = ActionSpec<unknown>;

export type ActionDialogProps = {
  open: boolean;
  spec: StoredActionSpec | null;
  status: ActionDialogStatus;
  error: ActionErrorClassification | null;
  busy: boolean;
  canRetry: boolean;
  canRetryUndo: boolean;
  onConfirm: () => void;
  onRetry: () => void;
  onUndo: () => void;
  onRetryUndo: () => void;
  onRequestClose: (reason?: DialogCloseReason) => void;
};

function errorLabel(kind: ActionErrorKind): string {
  switch (kind) {
    case "validation":
      return "请检查后重试";
    case "transient":
      return "暂时无法完成";
    case "postCommit":
      return "操作已完成，但后续处理失败";
    case "cancelled":
      return "操作已取消";
    case "permanent":
      return "操作失败";
  }
}

function fallbackMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim()) return error.message.trim().slice(0, 500);
  if (typeof error === "string" && error.trim()) return error.trim().slice(0, 500);
  return "操作未能完成，请稍后重试。";
}

function looksCancelled(error: unknown): boolean {
  if (error instanceof Error && error.name === "AbortError") return true;
  if (typeof error === "object" && error !== null && "code" in error) {
    const code = String((error as { code?: unknown }).code ?? "").toLowerCase();
    if (code === "cancelled" || code === "canceled" || code === "abort_err") return true;
  }
  const message = fallbackMessage(error).toLowerCase();
  return message === "cancelled" || message === "canceled" || message === "已取消";
}

export const defaultActionErrorClassifier: ActionErrorClassifier = (error) => ({
  kind: looksCancelled(error) ? "cancelled" : "permanent",
  message: fallbackMessage(error),
});

function classifySafely(
  classifier: ActionErrorClassifier | undefined,
  error: unknown,
  phase: ActionErrorContext["phase"],
): ActionErrorClassification {
  try {
    const classified = (classifier ?? defaultActionErrorClassifier)(error, { phase });
    if (classified && classified.message.trim()) return classified;
  } catch {
    // Error reporting must never replace the original action failure.
  }
  return defaultActionErrorClassifier(error, { phase });
}

export function ActionDialog({
  open,
  spec,
  status,
  error,
  busy,
  canRetry,
  canRetryUndo,
  onConfirm,
  onRetry,
  onUndo,
  onRetryUndo,
  onRequestClose,
}: ActionDialogProps) {
  if (!spec) return null;

  const running = status === "running";
  const undoing = status === "undoing";
  const closeLabel = status === "ready" || running ? (spec.cancelLabel ?? "取消") : "关闭";

  let primaryAction: ReactNode = null;
  if (status === "ready" || running) {
    primaryAction = (
      <button
        type="button"
        className={spec.tone === "danger" ? "danger" : "primary"}
        disabled={busy}
        onClick={onConfirm}
      >
        {running ? (spec.busyLabel ?? "处理中…") : (spec.confirmLabel ?? "确认")}
      </button>
    );
  } else if (status === "error" && canRetry) {
    primaryAction = (
      <button type="button" className="primary" onClick={onRetry}>
        重试
      </button>
    );
  } else if (status === "error" && error?.kind === "postCommit" && spec.undo) {
    primaryAction = (
      <button type="button" onClick={onUndo}>
        {spec.undo.label ?? "撤销"}
      </button>
    );
  } else if ((status === "succeeded" || status === "undoing") && spec.undo) {
    primaryAction = (
      <button type="button" disabled={busy} onClick={onUndo}>
        {undoing ? (spec.undo.busyLabel ?? "正在撤销…") : (spec.undo.label ?? "撤销")}
      </button>
    );
  } else if (status === "undoError" && canRetryUndo) {
    primaryAction = (
      <button type="button" onClick={onRetryUndo}>
        重试撤销
      </button>
    );
  }

  const actions = (
    <>
      <button type="button" disabled={busy} onClick={() => onRequestClose()}>
        {closeLabel}
      </button>
      {primaryAction}
    </>
  );

  return (
    <Dialog
      open={open}
      title={spec.title}
      description={spec.description}
      actions={actions}
      busy={busy}
      role={spec.role ?? (spec.tone === "danger" ? "alertdialog" : "dialog")}
      size="small"
      className="action-dialog"
      onRequestClose={onRequestClose}
    >
      {error ? (
        <div className={`action-dialog__message action-dialog__message--${error.kind}`} role="alert">
          <strong>{errorLabel(error.kind)}</strong>
          <p>{error.message}</p>
        </div>
      ) : null}
      {status === "succeeded" ? (
        <p className="action-dialog__success" role="status">
          {spec.completedDescription ?? "操作已完成。"}
        </p>
      ) : null}
    </Dialog>
  );
}

export type UseActionDialogResult = {
  openAction: <TResult>(spec: ActionSpec<TResult>) => boolean;
  closeAction: () => void;
  dialogProps: ActionDialogProps;
  dialog: ReactNode;
};

export function useActionDialog(): UseActionDialogResult {
  const [spec, setSpec] = useState<StoredActionSpec | null>(null);
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<ActionDialogStatus>("ready");
  const [error, setError] = useState<ActionErrorClassification | null>(null);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const undoBusyRef = useRef(false);
  const resultRef = useRef<unknown>(undefined);

  const resetAndClose = useCallback(() => {
    busyRef.current = false;
    undoBusyRef.current = false;
    setOpen(false);
    setSpec(null);
    setStatus("ready");
    setError(null);
    setBusy(false);
    resultRef.current = undefined;
  }, []);

  const closeAction = useCallback(() => {
    if (busyRef.current || undoBusyRef.current) return;
    resetAndClose();
  }, [resetAndClose]);

  const openAction = useCallback(<TResult,>(nextSpec: ActionSpec<TResult>): boolean => {
    if (busyRef.current || undoBusyRef.current) return false;
    resultRef.current = undefined;
    setSpec(nextSpec as unknown as StoredActionSpec);
    setStatus("ready");
    setError(null);
    setBusy(false);
    setOpen(true);
    return true;
  }, []);

  const run = useCallback(async () => {
    if (!spec || busyRef.current || undoBusyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setStatus("running");
    setError(null);

    let result: unknown;
    try {
      result = await spec.run();
    } catch (runError) {
      const classified = classifySafely(spec.classifyError, runError, "run");
      if (classified.kind === "cancelled") {
        resetAndClose();
      } else {
        setError(classified);
        setStatus("error");
      }
      busyRef.current = false;
      setBusy(false);
      return;
    }

    resultRef.current = result;
    try {
      await spec.afterSuccess?.(result);
    } catch (afterError) {
      const classified = classifySafely(spec.classifyError, afterError, "afterSuccess");
      setError({ kind: "postCommit", message: classified.message });
      setStatus("error");
      busyRef.current = false;
      setBusy(false);
      return;
    }

    busyRef.current = false;
    setBusy(false);
    if (spec.undo) {
      setStatus("succeeded");
    } else {
      resetAndClose();
    }
  }, [resetAndClose, spec]);

  const undo = useCallback(async () => {
    if (!spec?.undo || busyRef.current || undoBusyRef.current) return;
    undoBusyRef.current = true;
    setBusy(true);
    setStatus("undoing");
    setError(null);

    try {
      await spec.undo.run(resultRef.current);
    } catch (undoError) {
      const classified = classifySafely(
        spec.undo.classifyError ?? spec.classifyError,
        undoError,
        "undo",
      );
      if (classified.kind === "cancelled") {
        resetAndClose();
      } else {
        setError(classified);
        setStatus("undoError");
      }
      undoBusyRef.current = false;
      setBusy(false);
      return;
    }

    try {
      await spec.undo.afterSuccess?.(resultRef.current);
    } catch (afterError) {
      const classified = classifySafely(
        spec.undo.classifyError ?? spec.classifyError,
        afterError,
        "afterUndo",
      );
      setError({ kind: "postCommit", message: classified.message });
      setStatus("undoError");
      undoBusyRef.current = false;
      setBusy(false);
      return;
    }

    undoBusyRef.current = false;
    setBusy(false);
    resetAndClose();
  }, [resetAndClose, spec]);

  const canRetry = Boolean(spec?.retrySafe && error?.kind === "transient");
  const canRetryUndo = Boolean(spec?.undo?.retrySafe && error?.kind === "transient");
  const dialogProps = useMemo<ActionDialogProps>(() => ({
    open,
    spec,
    status,
    error,
    busy,
    canRetry,
    canRetryUndo,
    onConfirm: () => { void run(); },
    onRetry: () => { void run(); },
    onUndo: () => { void undo(); },
    onRetryUndo: () => { void undo(); },
    onRequestClose: closeAction,
  }), [busy, canRetry, canRetryUndo, closeAction, error, open, run, spec, status, undo]);

  return {
    openAction,
    closeAction,
    dialogProps,
    dialog: <ActionDialog {...dialogProps} />,
  };
}
