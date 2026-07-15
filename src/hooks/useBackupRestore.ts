import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../lib/tauriClient";

import {
  parseBackupText,
  type ApplicationBackupPayload,
  type BackupRestorePreview,
} from "../lib/backupRestore";

type BackupRestoreBusy = "preview" | "restore" | null;
type BackupInvoke = (
  command: string,
  args: { backup: ApplicationBackupPayload },
) => Promise<BackupRestorePreview>;
const defaultBackupInvoke: BackupInvoke = (command, args) => invoke<BackupRestorePreview>(command, args);

type UseBackupRestoreOptions = {
  backupText: string;
  onMessage: (message: string, isError?: boolean) => void;
  onRestored?: (preview: BackupRestorePreview) => void | Promise<void>;
  invokeCommand?: BackupInvoke;
};

export function useBackupRestore({
  backupText,
  onMessage,
  onRestored,
  invokeCommand = defaultBackupInvoke,
}: UseBackupRestoreOptions) {
  const [preview, setPreview] = useState<BackupRestorePreview | null>(null);
  const [busy, setBusy] = useState<BackupRestoreBusy>(null);
  const operationLockedRef = useRef(false);
  const previewedTextRef = useRef<string | null>(null);
  const currentTextRef = useRef(backupText);
  currentTextRef.current = backupText;

  const resetPreview = useCallback(() => {
    previewedTextRef.current = null;
    setPreview(null);
  }, []);

  useEffect(() => {
    if (previewedTextRef.current !== null && previewedTextRef.current !== backupText) {
      resetPreview();
    }
  }, [backupText, resetPreview]);

  const inspect = useCallback(async () => {
    if (operationLockedRef.current) return;
    operationLockedRef.current = true;
    setBusy("preview");
    const inspectedText = backupText;
    try {
      const backup = parseBackupText(inspectedText);
      const nextPreview = await invokeCommand("backup_preview_restore", { backup });
      if (currentTextRef.current !== inspectedText) return;
      previewedTextRef.current = inspectedText;
      setPreview(nextPreview);
      onMessage("备份校验通过，请核对覆盖数量后确认恢复。");
    } catch (error) {
      onMessage(String(error), true);
    } finally {
      operationLockedRef.current = false;
      setBusy(null);
    }
  }, [backupText, invokeCommand, onMessage]);

  const restore = useCallback(async () => {
    if (
      operationLockedRef.current
    ) return null;
    if (!preview || previewedTextRef.current !== backupText) {
      throw new Error("备份内容尚未通过当前版本校验，请先重新检查备份。");
    }
    operationLockedRef.current = true;
    setBusy("restore");
    try {
      const backup = parseBackupText(backupText);
      const restoredPreview = await invokeCommand("backup_restore_atomic", { backup });
      previewedTextRef.current = backupText;
      setPreview(restoredPreview);
      await onRestored?.(restoredPreview);
      return restoredPreview;
    } finally {
      operationLockedRef.current = false;
      setBusy(null);
    }
  }, [backupText, invokeCommand, onRestored, preview]);

  return { preview, busy, inspect, restore, resetPreview };
}
