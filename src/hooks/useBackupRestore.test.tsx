// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { BackupRestorePreview } from "../lib/backupRestore";
import { useBackupRestore } from "./useBackupRestore";

const BACKUP_TEXT = JSON.stringify({
  version: 2,
  library: { version: 2, tracks: [], playlists: [] },
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

  it("locks duplicate atomic restores and returns the restored preview", async () => {
    const pendingRestore = deferred<BackupRestorePreview>();
    const invokeCommand = vi.fn((command: string) => command === "backup_preview_restore"
      ? Promise.resolve({ trackCount: 8, playlistCount: 2, sourceCount: 1 })
      : pendingRestore.promise);
    const { result } = renderHook(() => useBackupRestore({
      backupText: BACKUP_TEXT,
      invokeCommand,
      onMessage: vi.fn(),
    }));

    await act(async () => result.current.inspect());
    act(() => {
      void result.current.restore();
      void result.current.restore();
    });

    expect(result.current.busy).toBe("restore");
    expect(invokeCommand.mock.calls.filter(([command]) => command === "backup_restore_atomic")).toHaveLength(1);

    await act(async () => pendingRestore.resolve({ trackCount: 8, playlistCount: 2, sourceCount: 1 }));
    expect(result.current.busy).toBeNull();
    expect(result.current.preview).toEqual({ trackCount: 8, playlistCount: 2, sourceCount: 1 });
  });

  it("propagates atomic restore failures so the action dialog can classify and retry", async () => {
    const invokeCommand = vi.fn((command: string) => command === "backup_preview_restore"
      ? Promise.resolve({ trackCount: 1, playlistCount: 0, sourceCount: 0 })
      : Promise.reject(new Error("source backup is corrupt")));
    const onMessage = vi.fn();
    const { result } = renderHook(() => useBackupRestore({
      backupText: BACKUP_TEXT,
      invokeCommand,
      onMessage,
    }));

    await act(async () => result.current.inspect());
    await expect(act(async () => result.current.restore())).rejects.toThrow("source backup is corrupt");

    expect(result.current.busy).toBeNull();
    expect(onMessage).not.toHaveBeenCalledWith(expect.stringContaining("corrupt"), true);
  });
});
