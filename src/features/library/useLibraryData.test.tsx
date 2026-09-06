// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useLibraryData } from "./useLibraryData";
import type { LibraryTrack, ViewId } from "../../types";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("../../lib/tauriClient", () => ({ invoke }));
const track: LibraryTrack = { id: 1, path: "C:/Music/a.flac", title: "Track", artist: "Artist", album: "", durationSeconds: 60, favorite: false, addedAtMs: 0, missing: false };

describe("library data", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command) => command === "library_tracks" ? [track] : []);
  });
  afterEach(cleanup);

  it("shows the indexed library before starting the filesystem scan", async () => {
    let finish!: (tracks: LibraryTrack[]) => void;
    const base = invoke.getMockImplementation();
    invoke.mockImplementation((command) => command === "library_scan_missing"
      ? new Promise((resolve) => { finish = resolve; }) : base?.(command));
    const { result, rerender } = renderHook(({ view }: { view: ViewId }) => useLibraryData(view), { initialProps: { view: "discovery" as ViewId } });
    await waitFor(() => expect(result.current.library).toHaveLength(1));
    expect(invoke).not.toHaveBeenCalledWith("library_scan_missing");
    rerender({ view: "library" });
    expect(result.current.libraryLoadState).toBe("ready");
    await act(async () => { finish([{ ...track, title: "Stale title", missing: true }]); });
    expect(result.current.library[0]).toMatchObject({ title: "Track", missing: true });
  });

  it("does not resurrect a removed track when an older scan completes", async () => {
    let finish!: (tracks: LibraryTrack[]) => void;
    const base = invoke.getMockImplementation();
    invoke.mockImplementation((command) => command === "library_scan_missing"
      ? new Promise((resolve) => { finish = resolve; }) : base?.(command));
    const { result } = renderHook(() => useLibraryData("library"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_scan_missing"));
    invoke.mockImplementation(async () => []);
    await act(async () => { await result.current.refreshLibrary(); });
    await act(async () => { finish([{ ...track, missing: true }]); });
    expect(result.current.library).toEqual([]);
  });
});
