// @vitest-environment jsdom
import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
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
});
