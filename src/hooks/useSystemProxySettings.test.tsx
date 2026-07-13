// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProxyStatus } from "../types";
import { useSystemProxySettings } from "./useSystemProxySettings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const automatic: ProxyStatus = { mode: "auto", detected: true, effective: true };

afterEach(() => {
  vi.clearAllMocks();
});

describe("useSystemProxySettings", () => {
  it("loads status and applies manual or automatic modes returned by the backend", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(automatic)
      .mockResolvedValueOnce({ mode: "off", detected: true, effective: false })
      .mockResolvedValueOnce(automatic);
    const onError = vi.fn();
    const { result } = renderHook(() => useSystemProxySettings(onError));

    await waitFor(() => expect(result.current.status).toEqual(automatic));
    await act(async () => { await result.current.setMode("off"); });
    expect(invoke).toHaveBeenLastCalledWith("network_set_proxy_mode", { mode: "off" });
    expect(result.current.status?.effective).toBe(false);

    await act(async () => { await result.current.setMode("auto"); });
    expect(result.current.status).toEqual(automatic);
    expect(onError).not.toHaveBeenCalled();
  });

  it("keeps the prior status when a mode update fails", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(automatic)
      .mockRejectedValueOnce(new Error("write failed"));
    const onError = vi.fn();
    const { result } = renderHook(() => useSystemProxySettings(onError));
    await waitFor(() => expect(result.current.status).toEqual(automatic));

    await act(async () => { await result.current.setMode("off"); });
    expect(result.current.status).toEqual(automatic);
    expect(onError).toHaveBeenCalledTimes(1);
    expect(result.current.busy).toBe(false);
  });
});
