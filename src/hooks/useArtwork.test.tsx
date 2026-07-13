// @vitest-environment jsdom
import { renderHook, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useArtworkUrl } from "./useArtwork";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("useArtworkUrl", () => {
  it("passes through safe image data without invoking the backend", () => {
    const dataUrl = "data:image/png;base64,cG5n";
    const { result } = renderHook(() => useArtworkUrl(dataUrl));
    expect(result.current).toBe(dataUrl);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("never exposes a remote URL and shares concurrent fetches", async () => {
    const response = deferred<{ mime: string; dataUrl: string }>();
    vi.mocked(invoke).mockReturnValue(response.promise as never);
    const url = "https://images.invalid/shared-cover.jpg";
    const first = renderHook(() => useArtworkUrl(url));
    const second = renderHook(() => useArtworkUrl(url));

    expect(first.result.current).toBeNull();
    expect(second.result.current).toBeNull();
    expect(invoke).toHaveBeenCalledTimes(1);
    response.resolve({ mime: "image/jpeg", dataUrl: "data:image/jpeg;base64,anBlZw==" });
    await waitFor(() => expect(first.result.current).toMatch(/^data:image\/jpeg/));
    await waitFor(() => expect(second.result.current).toBe(first.result.current));
  });

  it("ignores a slow response after the source URL changes", async () => {
    const older = deferred<{ mime: string; dataUrl: string }>();
    vi.mocked(invoke)
      .mockReturnValueOnce(older.promise as never)
      .mockResolvedValueOnce({ mime: "image/png", dataUrl: "data:image/png;base64,bmV3" });
    const { result, rerender } = renderHook(
      ({ url }) => useArtworkUrl(url),
      { initialProps: { url: "https://images.invalid/older.jpg" } },
    );
    rerender({ url: "https://images.invalid/latest.png" });
    await waitFor(() => expect(result.current).toBe("data:image/png;base64,bmV3"));

    older.resolve({ mime: "image/jpeg", dataUrl: "data:image/jpeg;base64,b2xk" });
    await Promise.resolve();
    expect(result.current).toBe("data:image/png;base64,bmV3");
  });
});
