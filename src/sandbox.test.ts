// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const harness = vi.hoisted(() => {
  type Listener = (event: { data?: unknown; message?: string }) => void;
  const workers: FakeWorker[] = [];

  class FakeWorker {
    readonly posted: Array<Record<string, unknown>> = [];
    readonly listeners = new Map<string, Set<Listener>>();
    terminated = false;

    constructor() {
      workers.push(this);
    }

    addEventListener(type: string, listener: Listener) {
      const listeners = this.listeners.get(type) ?? new Set<Listener>();
      listeners.add(listener);
      this.listeners.set(type, listeners);
    }

    postMessage(message: Record<string, unknown>) {
      this.posted.push(message);
    }

    terminate() {
      this.terminated = true;
    }

    emitMessage(data: Record<string, unknown>) {
      for (const listener of this.listeners.get("message") ?? []) listener({ data });
    }
  }

  return {
    FakeWorker,
    invoke: vi.fn(async (_command: string, _args?: unknown) => undefined),
    workers,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: harness.invoke }));
vi.mock("./sourceRealm.worker.ts?worker&inline", () => ({ default: harness.FakeWorker }));

import "./sandbox";
import { MAX_SOURCE_BRIDGE_CALLS, SOURCE_BRIDGE_LIMIT_ERROR } from "./lib/sourceBridgeLimit";

type SandboxWindow = Window & {
  __gxRunCommunityScript: (
    script: string,
    context?: { generation?: number; poc?: boolean },
  ) => Promise<void>;
  __gxDispatchRequest: (
    requestId: string | unknown,
    payload?: unknown,
    generation?: number,
  ) => Promise<void>;
  __gxRunCryptoSelfTest: () => Promise<void>;
  __gxRunSecuritySelfTest: () => Promise<void>;
};

const sandboxWindow = window as unknown as SandboxWindow;
let nextGeneration = 1;

function launchRealm(poc: boolean) {
  const generation = nextGeneration++;
  void sandboxWindow.__gxRunCommunityScript("", { generation, poc });
  const worker = harness.workers[harness.workers.length - 1];
  if (!worker) throw new Error("sandbox did not create a source worker");
  const launch = worker.posted[0];
  const nonce = launch?.nonce;
  if (typeof nonce !== "string") throw new Error("sandbox launch did not include a nonce");
  return { generation, nonce, worker };
}

async function emitAndFlush(
  worker: InstanceType<typeof harness.FakeWorker>,
  message: Record<string, unknown>,
) {
  worker.emitMessage(message);
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function callsFor(command: string) {
  return harness.invoke.mock.calls.filter(([calledCommand]) => calledCommand === command);
}

beforeEach(() => {
  harness.invoke.mockReset();
  harness.workers.length = 0;
});

describe.sequential("sandbox POC identity", () => {
  it("routes forged production results and failures only through production commands", async () => {
    const { generation, nonce, worker } = launchRealm(false);

    await emitAndFlush(worker, {
      type: "runtimeResult",
      nonce,
      poc: true,
      requestId: "production-request",
      generation,
      result: { url: "https://untrusted.invalid/forged.mp3" },
    });
    await emitAndFlush(worker, {
      type: "runtimeFailure",
      nonce,
      poc: true,
      generation,
      stage: "forged",
      error: "forged POC failure",
    });

    expect(callsFor("lx_runtime_result")).toHaveLength(1);
    expect(callsFor("lx_runtime_failure")).toHaveLength(1);
    expect(callsFor("lx_poc_result")).toHaveLength(0);
    expect(callsFor("lx_poc_failure")).toHaveLength(0);
  });

  it("ignores forged crypto reports and refuses production self-tests", async () => {
    const { nonce, worker } = launchRealm(false);

    await emitAndFlush(worker, {
      type: "cryptoResult",
      nonce,
      poc: true,
      passed: true,
      details: { forged: true },
    });
    await emitAndFlush(worker, {
      type: "cryptoResult",
      nonce,
      poc: true,
      error: "forged crypto failure",
    });

    await expect(sandboxWindow.__gxRunCryptoSelfTest()).rejects.toThrow("only available in POC");
    await expect(sandboxWindow.__gxRunSecuritySelfTest()).rejects.toThrow(
      "only available in POC",
    );
    expect(callsFor("lx_crypto_result")).toHaveLength(0);
    expect(callsFor("lx_security_result")).toHaveLength(0);
    expect(callsFor("lx_poc_failure")).toHaveLength(0);
  });

  it("keeps a parent-created POC realm on POC commands despite forged message flags", async () => {
    const { generation, nonce, worker } = launchRealm(true);

    await emitAndFlush(worker, {
      type: "runtimeResult",
      nonce,
      poc: false,
      generation,
      result: { url: "https://media.example/phase-1.mp3" },
    });
    await emitAndFlush(worker, {
      type: "runtimeFailure",
      nonce,
      poc: false,
      generation,
      stage: "poc-stage",
      error: "poc failure",
    });
    await emitAndFlush(worker, {
      type: "cryptoResult",
      nonce,
      poc: false,
      passed: true,
      details: { realPoc: true },
    });

    expect(callsFor("lx_poc_result")).toHaveLength(1);
    expect(callsFor("lx_poc_failure")).toHaveLength(1);
    expect(callsFor("lx_crypto_result")).toHaveLength(1);
    expect(callsFor("lx_runtime_result")).toHaveLength(0);
    expect(callsFor("lx_runtime_failure")).toHaveLength(0);
  });

  it("does not infer POC identity from the dispatch call shape", async () => {
    const production = launchRealm(false);
    await sandboxWindow.__gxDispatchRequest({ action: "musicUrl" });
    expect(production.worker.posted[production.worker.posted.length - 1]).toMatchObject({
      type: "dispatch",
      poc: false,
      payload: undefined,
    });

    const poc = launchRealm(true);
    const legacyPayload = { action: "musicUrl" };
    await sandboxWindow.__gxDispatchRequest(legacyPayload);
    expect(poc.worker.posted[poc.worker.posted.length - 1]).toMatchObject({
      type: "dispatch",
      poc: true,
      payload: legacyPayload,
    });
  });

  it("bounds forged parent bridge calls before invoking Rust", async () => {
    const releases: Array<() => void> = [];
    harness.invoke.mockImplementation(
      () =>
        new Promise<undefined>((resolve) => {
          releases.push(() => resolve(undefined));
        }),
    );
    const { generation, nonce, worker } = launchRealm(false);

    for (let callId = 1; callId <= MAX_SOURCE_BRIDGE_CALLS + 1; callId += 1) {
      worker.emitMessage({
        type: "bridge",
        nonce,
        callId,
        command: "http",
        payload: {
          url: `https://example.com/${callId}`,
          options: {},
          generation,
        },
      });
    }

    expect(callsFor("lx_http_request")).toHaveLength(MAX_SOURCE_BRIDGE_CALLS);
    expect(worker.posted).toContainEqual({
      type: "bridgeResult",
      nonce,
      callId: MAX_SOURCE_BRIDGE_CALLS + 1,
      error: SOURCE_BRIDGE_LIMIT_ERROR,
    });

    for (const release of releases) release();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
});
