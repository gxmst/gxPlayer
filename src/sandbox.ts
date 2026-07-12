import { invoke } from "@tauri-apps/api/core";
import SourceRealmWorker from "./sourceRealm.worker.ts?worker&inline";

type LxHttpResponse = {
  statusCode: number;
  headers: Record<string, string>;
  body: unknown;
};

type RealmMessage = {
  type: string;
  nonce?: string;
  callId?: number;
  command?: "http" | "send";
  payload?: Record<string, unknown>;
  requestId?: string | unknown;
  generation?: number;
  result?: unknown;
  error?: string | null;
  stage?: string;
  poc?: boolean;
  passed?: boolean;
  details?: unknown;
};

let realmWorker: Worker | null = null;
let realmNonce = "";
let currentGeneration = 0;
let pocMode = false;

function randomNonce(): string {
  const bytes = new Uint8Array(24);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function destroySourceRealm(): void {
  realmNonce = "";
  if (realmWorker) {
    realmWorker.terminate();
    realmWorker = null;
  }
}

function isActiveRealm(worker: Worker, nonce: string): boolean {
  return realmWorker === worker && realmNonce === nonce;
}

async function reportRealmFailure(stage: string, error: unknown, generation: number, poc: boolean) {
  if (poc) {
    await invoke("lx_poc_failure", { stage, error: String(error) });
  } else {
    await invoke("lx_runtime_failure", { generation, stage, error: String(error) });
  }
}

async function handleBridgeCall(worker: Worker, nonce: string, message: RealmMessage) {
  const callId = message.callId;
  if (typeof callId !== "number" || !message.command || !message.payload) return;
  try {
    let result: unknown;
    if (message.command === "http") {
      result = await invoke<LxHttpResponse>("lx_http_request", {
        url: message.payload.url,
        options: message.payload.options ?? {},
      });
    } else if (message.command === "send") {
      const eventName = message.payload.eventName;
      if (eventName !== "inited" && eventName !== "updateAlert") {
        throw new Error(`Unsupported LX send event: ${String(eventName)}`);
      }
      result = await invoke("lx_send", {
        eventName,
        data: message.payload.data,
        generation: message.payload.generation,
      });
    } else {
      throw new Error("Unsupported source-realm bridge command");
    }
    if (isActiveRealm(worker, nonce)) {
      worker.postMessage({ type: "bridgeResult", nonce, callId, result: result ?? null });
    }
  } catch (error) {
    if (isActiveRealm(worker, nonce)) {
      worker.postMessage({ type: "bridgeResult", nonce, callId, error: String(error) });
    }
  }
}

async function handleRealmMessage(worker: Worker, nonce: string, message: RealmMessage) {
  if (!isActiveRealm(worker, nonce) || message.nonce !== nonce) return;
  switch (message.type) {
    case "bridge":
      await handleBridgeCall(worker, nonce, message);
      break;
    case "runtimeResult":
      if (message.poc) {
        if (message.error) {
          await invoke("lx_poc_failure", { stage: "music-url", error: message.error });
        } else {
          await invoke("lx_poc_result", { result: message.result });
        }
      } else {
        await invoke("lx_runtime_result", {
          requestId: message.requestId,
          generation: message.generation,
          result: message.error ? null : message.result,
          error: message.error ?? null,
        });
      }
      break;
    case "runtimeFailure":
      await reportRealmFailure(
        message.stage ?? "community-script",
        message.error ?? "unknown source realm failure",
        message.generation ?? currentGeneration,
        Boolean(message.poc),
      );
      break;
    case "cryptoResult":
      if (message.error) {
        await invoke("lx_poc_failure", { stage: "sync-crypto", error: message.error });
      } else {
        await invoke("lx_crypto_result", {
          passed: Boolean(message.passed),
          details: message.details ?? {},
        });
      }
      break;
  }
}

function createSourceRealm(
  script: string,
  context: {
    generation?: number;
    poc?: boolean;
    scriptInfo?: Record<string, unknown>;
    config?: Record<string, unknown>;
  },
): void {
  destroySourceRealm();
  const nonce = randomNonce();
  const worker = new SourceRealmWorker({ name: "gx-lx-source-realm" });
  realmWorker = worker;
  realmNonce = nonce;
  worker.addEventListener("message", (event: MessageEvent<RealmMessage>) => {
    void handleRealmMessage(worker, nonce, event.data).catch((error) => {
      if (isActiveRealm(worker, nonce)) console.error("LX source bridge failed", error);
    });
  });
  worker.addEventListener("error", (event) => {
    if (!isActiveRealm(worker, nonce)) return;
    void reportRealmFailure(
      "source-worker",
      event.message || "LX source worker crashed",
      currentGeneration,
      pocMode,
    );
  });
  worker.postMessage({ type: "launch", nonce, script, context });
}

Object.assign(window, {
  async __gxRunCommunityScript(
    script: string,
    context: {
      generation?: number;
      poc?: boolean;
      scriptInfo?: Record<string, unknown>;
      config?: Record<string, unknown>;
    } = {},
  ) {
    currentGeneration = context.generation ?? 0;
    pocMode = context.poc ?? false;
    createSourceRealm(script, context);
  },
  async __gxDispatchRequest(requestId: string | unknown, payload?: unknown, generation?: number) {
    const worker = realmWorker;
    const nonce = realmNonce;
    if (!worker || !nonce) throw new Error("LX source realm is unavailable");
    const isPocRequest = payload === undefined;
    worker.postMessage({
      type: "dispatch",
      nonce,
      requestId,
      payload: isPocRequest ? requestId : payload,
      generation: generation ?? currentGeneration,
      poc: isPocRequest || pocMode,
    });
  },
  async __gxRunCryptoSelfTest() {
    const worker = realmWorker;
    const nonce = realmNonce;
    if (!worker || !nonce) {
      await invoke("lx_poc_failure", {
        stage: "sync-crypto",
        error: "LX source realm is unavailable",
      });
      return;
    }
    worker.postMessage({ type: "cryptoSelfTest", nonce });
  },
  async __gxRunSecuritySelfTest() {
    const results = {
      mainCommandBlocked: false,
      sourceCommandBlocked: false,
      openerBlocked: false,
      newWindowBlocked: false,
      fileBlocked: false,
      shellBlocked: false,
      clipboardBlocked: false,
      ssrfBlocked: false,
    };
    try {
      await invoke("main_only_probe");
    } catch {
      results.mainCommandBlocked = true;
    }
    try {
      await invoke("source_list");
    } catch {
      results.sourceCommandBlocked = true;
    }
    try {
      await invoke("plugin:opener|open_url", { url: "https://example.com" });
    } catch {
      results.openerBlocked = true;
    }
    try {
      results.newWindowBlocked = window.open("https://example.com", "_blank") === null;
    } catch {
      results.newWindowBlocked = true;
    }
    try {
      await invoke("plugin:fs|read_text_file", { path: "C:\\Windows\\win.ini" });
    } catch {
      results.fileBlocked = true;
    }
    try {
      await invoke("plugin:shell|execute", { program: "cmd.exe", args: ["/c", "ver"] });
    } catch {
      results.shellBlocked = true;
    }
    try {
      await invoke("plugin:clipboard-manager|read_text");
    } catch {
      results.clipboardBlocked = true;
    }
    try {
      await invoke("lx_http_request", { url: "http://127.0.0.1/private", options: {} });
    } catch {
      results.ssrfBlocked = true;
    }
    await invoke("lx_security_result", { results });
  },
});

window.addEventListener("beforeunload", destroySourceRealm, { once: true });

void invoke("sandbox_ready").catch((error) => {
  console.error("Failed to announce LX sandbox readiness", error);
});
