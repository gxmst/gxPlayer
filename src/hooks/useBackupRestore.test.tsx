// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { BackupRestorePreview } from "../lib/backupRestore";
import { useBackupRestore } from "./useBackupRestore";

const BACKUP_TEXT = JSON.stringify({
  version: 1,
  library: { version: 1, tracks: [], playlists: [] },
  sources: { version: 1, sources: [] },
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return { promise, resolve };
}

describe("useBackupRestore", () => {
  it("previews once while busy and exposes the validated overwrite counts", async () => {
    const pending = deferred<BackupRestorePreview>();
    const invokeCommand = vi.fn(() => pending.promise);
    const onMessage = vi.fn();
    const { result } = renderHook(() => useBackupRestore({
      backupText: BACKUP_TEXT,
      invokeCommand,
      confirmRestore: vi.fn(() => true),
      onRestored: vi.fn(async () => undefined),
      onMessage,
    }));

    act(() => {
      void result.current.inspect();
      void result.current.inspect();
    });
    expect(result.current.busy).toBe("preview");
    expect(invokeCommand).toHaveBeenCalledTimes(1);

    await act(async () => pending.resolve({ trackCount: 8, playlistCount: 2, sourceCount: 1 }));
    expect(result.current.busy).toBeNull();
    expect(result.current.preview).toEqual({ trackCount: 8, playlistCount: 2, sourceCount: 1 });
  });

  it("requires confirmation and locks duplicate atomic restores", async () => {
    const pendingRestore = deferred<BackupRestorePreview>();
    const invokeCommand = vi.fn((command: string) => command === "backup_preview_restore"
      ? Promise.resolve({ trackCount: 8, playlistCount: 2, sourceCount: 1 })
      : pendingRestore.promise);
    const confirmRestore = vi.fn(() => true);
    const onRestored = vi.fn(async () => undefined);
    const { result } = renderHook(() => useBackupRestore({
      backupText: BACKUP_TEXT,
      invokeCommand,
      confirmRestore,
      onRestored,
      onMessage: vi.fn(),
    }));

    await act(async () => result.current.inspect());
    act(() => {
      void result.current.restore();
      void result.current.restore();
    });

    expect(result.current.busy).toBe("restore");
    expect(confirmRestore).toHaveBeenCalledTimes(1);
    expect(invokeCommand.mock.calls.filter(([command]) => command === "backup_restore_atomic")).toHaveLength(1);

    await act(async () => pendingRestore.resolve({ trackCount: 8, playlistCount: 2, sourceCount: 1 }));
    expect(result.current.busy).toBeNull();
    expect(onRestored).toHaveBeenCalledTimes(1);
  });
});
