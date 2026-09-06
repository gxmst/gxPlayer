// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EMPTY_ENGINE, type EngineSnapshot } from "../types";
import { useEngineSnapshot } from "./useEngineSnapshot";

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function Probe() {
  const [snapshot] = useEngineSnapshot(() => undefined);
  return <output>{`${snapshot.status}:${snapshot.positionSeconds}`}</output>;
}

describe("useEngineSnapshot", () => {
  afterEach(() => { cleanup(); vi.useRealTimers(); });
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
  });

  it("does not let a delayed poll overwrite a newer pushed snapshot", async () => {
    const poll = deferred<EngineSnapshot>();
    let onSnapshot: ((event: { payload: EngineSnapshot }) => void) | undefined;
    invoke.mockReturnValueOnce(poll.promise);
    listen.mockImplementation((_event, callback) => {
      onSnapshot = callback;
      return Promise.resolve(() => undefined);
    });

    render(<Probe />);
    const pushed = { ...EMPTY_ENGINE, status: "playing" as const, positionSeconds: 12 };
    await act(async () => {
      onSnapshot?.({ payload: pushed });
      poll.resolve({ ...EMPTY_ENGINE, status: "loading", positionSeconds: 2 });
      await poll.promise;
    });

    expect(screen.getByText("playing:12")).toBeTruthy();
  });

  it("skips redundant polls while pushes are live and resumes polling when they stop", async () => {
    vi.useFakeTimers();
    let push!: (event: { payload: EngineSnapshot }) => void;
    invoke.mockResolvedValue(EMPTY_ENGINE);
    listen.mockImplementation((_event, handler) => { push = handler; return Promise.resolve(() => undefined); });
    render(<Probe />);
    await act(async () => undefined);
    for (let tick = 0; tick < 8; tick++) {
      await act(async () => {
        push({ payload: { ...EMPTY_ENGINE, status: "playing", positionSeconds: tick } });
        await vi.advanceTimersByTimeAsync(250);
      });
    }
    expect(invoke).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("keeps unchanged queue and DSP references while position advances", async () => {
    let push!: (event: { payload: EngineSnapshot }) => void;
    invoke.mockResolvedValue(EMPTY_ENGINE);
    listen.mockImplementation((_event, handler) => { push = handler; return Promise.resolve(() => undefined); });
    const { result } = renderHook(() => useEngineSnapshot(() => undefined));
    await act(async () => undefined);
    const initial = result.current[0];
    await act(async () => { push({ payload: JSON.parse(JSON.stringify(initial)) as EngineSnapshot }); });
    expect(result.current[0]).toBe(initial);
    await act(async () => { push({ payload: { ...JSON.parse(JSON.stringify(initial)), positionSeconds: 2 } as EngineSnapshot }); });
    expect(result.current[0].positionSeconds).toBe(2);
    expect(result.current[0].queue).toBe(initial.queue);
    expect(result.current[0].dspSettings).toBe(initial.dspSettings);
  });
});
