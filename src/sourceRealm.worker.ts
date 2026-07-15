import { Buffer } from "buffer";
import CryptoJS from "crypto-js";
import forge from "node-forge";
import * as pako from "pako";
import {
  SOURCE_BRIDGE_LIMIT_ERROR,
  hasSourceBridgeCapacity,
} from "./lib/sourceBridgeLimit";

type RequestHandler = (payload: unknown) => unknown | Promise<unknown>;

type LxHttpResponse = {
  statusCode: number;
  headers: Record<string, string>;
  body: unknown;
};

type ParentMessage = {
  type: "launch" | "dispatch" | "bridgeResult" | "cryptoSelfTest";
  nonce: string;
  script?: string;
  context?: {
    generation?: number;
    poc?: boolean;
    scriptInfo?: Record<string, unknown>;
    config?: Record<string, unknown>;
  };
  requestId?: string | unknown;
  payload?: unknown;
  generation?: number;
  poc?: boolean;
  callId?: number;
  result?: unknown;
  error?: string;
};

const EVENT_NAMES = {
  request: "request",
  inited: "inited",
  updateAlert: "updateAlert",
} as const;

const NETWORK_DISABLED_MESSAGE = "direct network access is disabled; use lx.request";

function denyNetwork(name: string): never {
  throw new Error(`${name}: ${NETWORK_DISABLED_MESSAGE}`);
}

function installNetworkLockdown(): void {
  const blockedFunction = () => denyNetwork("network API");
  const blockedConstructor = function (this: unknown): never {
    return denyNetwork("network API");
  };
  const properties: Array<[string, unknown]> = [
    ["fetch", blockedFunction],
    ["WebSocket", blockedConstructor],
    ["EventSource", blockedConstructor],
    ["XMLHttpRequest", blockedConstructor],
    ["Worker", blockedConstructor],
    ["SharedWorker", blockedConstructor],
    ["WebTransport", blockedConstructor],
    ["importScripts", blockedFunction],
  ];
  for (const [name, value] of properties) {
    try {
      Object.defineProperty(globalThis, name, {
        configurable: false,
        enumerable: false,
        writable: false,
        value,
      });
    } catch (error) {
      throw new Error(`failed to disable ${name}: ${String(error)}`);
    }
    if ((globalThis as Record<string, unknown>)[name] !== value) {
      throw new Error(`failed to verify that ${name} is disabled`);
    }
  }
}

let nonce = "";
let currentGeneration = 0;
let pocMode = false;
let requestHandler: RequestHandler | null = null;
let nextCallId = 1;
const pendingBridge = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (error: Error) => void }
>();

function post(message: Record<string, unknown>): void {
  self.postMessage({ ...message, nonce });
}

function bridge(command: "http" | "send", payload: Record<string, unknown>): Promise<unknown> {
  if (!hasSourceBridgeCapacity(pendingBridge.size)) {
    return Promise.reject(new Error(SOURCE_BRIDGE_LIMIT_ERROR));
  }
  const callId = nextCallId++;
  return new Promise((resolve, reject) => {
    pendingBridge.set(callId, { resolve, reject });
    post({ type: "bridge", callId, command, payload });
  });
}

function toBuffer(value: Buffer | Uint8Array | string): Buffer {
  return Buffer.isBuffer(value) ? value : Buffer.from(value);
}

function toWordArray(value: Buffer | Uint8Array | string): CryptoJS.lib.WordArray {
  const bytes = toBuffer(value);
  const words: number[] = [];
  for (let index = 0; index < bytes.length; index += 1) {
    words[index >>> 2] = (words[index >>> 2] || 0) | (bytes[index] << (24 - (index % 4) * 8));
  }
  return CryptoJS.lib.WordArray.create(words, bytes.length);
}

function fromWordArray(value: CryptoJS.lib.WordArray): Buffer {
  const output = Buffer.alloc(value.sigBytes);
  for (let index = 0; index < value.sigBytes; index += 1) {
    output[index] = (value.words[index >>> 2] >>> (24 - (index % 4) * 8)) & 0xff;
  }
  return output;
}

function installLxContract(scriptInfo?: Record<string, unknown>): void {
  requestHandler = null;
  const lx = {
    EVENT_NAMES,
    version: "2.0.0",
    env: "desktop",
    currentScriptInfo: scriptInfo ?? {
      name: "Phase-1 community compatibility script",
      version: "external",
      author: "external",
      homepage: "",
      rawScript: "",
    },
    request(
      url: string,
      options: Record<string, unknown> = {},
      callback: (error: Error | null, response?: LxHttpResponse, body?: unknown) => void,
    ) {
      let cancelled = false;
      void bridge("http", { url, options, generation: currentGeneration })
        .then((response) => {
          if (!cancelled) {
            const typed = response as LxHttpResponse;
            callback(null, typed, typed.body);
          }
        })
        .catch((error) => {
          if (!cancelled) callback(new Error(String(error)));
        });
      return () => {
        cancelled = true;
      };
    },
    on(eventName: string, handler: RequestHandler) {
      if (eventName !== EVENT_NAMES.request) {
        return Promise.reject(new Error(`Unsupported event: ${eventName}`));
      }
      requestHandler = handler;
      return Promise.resolve();
    },
    send(eventName: string, data: unknown) {
      if (eventName !== EVENT_NAMES.inited && eventName !== EVENT_NAMES.updateAlert) {
        return Promise.reject(new Error(`Unsupported event: ${eventName}`));
      }
      return bridge("send", { eventName, data, generation: currentGeneration });
    },
    utils: {
      crypto: {
        aesEncrypt(
          data: Buffer | Uint8Array | string,
          algorithm: string,
          key: Buffer | Uint8Array | string,
          iv: Buffer | Uint8Array | string,
        ) {
          const mode = algorithm.toLowerCase().includes("ecb")
            ? CryptoJS.mode.ECB
            : CryptoJS.mode.CBC;
          const encrypted = CryptoJS.AES.encrypt(toWordArray(data), toWordArray(key), {
            iv: toWordArray(iv),
            mode,
            padding: CryptoJS.pad.Pkcs7,
          });
          return fromWordArray(encrypted.ciphertext);
        },
        rsaEncrypt(data: Buffer | Uint8Array | string, publicKeyPem: string) {
          const input = toBuffer(data);
          if (input.length > 128) throw new Error("RSA raw input exceeds 128 bytes");
          const padded = Buffer.concat([Buffer.alloc(128 - input.length), input]);
          const publicKey = forge.pki.publicKeyFromPem(publicKeyPem);
          const encrypted = publicKey.encrypt(padded.toString("binary"), "RAW");
          return Buffer.from(encrypted, "binary");
        },
        randomBytes(size: number) {
          const output = Buffer.alloc(size);
          crypto.getRandomValues(output);
          return output;
        },
        md5(value: Buffer | Uint8Array | string) {
          return CryptoJS.MD5(toWordArray(value)).toString(CryptoJS.enc.Hex);
        },
      },
      buffer: {
        from(value: unknown, encoding?: BufferEncoding) {
          return Buffer.from(value as never, encoding);
        },
        bufToString(value: Buffer | Uint8Array | string, encoding?: BufferEncoding) {
          return toBuffer(value).toString(encoding);
        },
      },
      zlib: {
        async inflate(value: Buffer | Uint8Array) {
          return Buffer.from(pako.inflate(value));
        },
        async deflate(value: Buffer | Uint8Array | string) {
          return Buffer.from(pako.deflate(toBuffer(value)));
        },
      },
    },
  };
  Object.assign(globalThis, {
    Buffer,
    lx,
    // Community sources commonly reference window.lx. This alias stays inside the worker and
    // cannot reach the parent WebView or its Tauri bridge.
    window: globalThis,
  });
}

async function launch(message: ParentMessage): Promise<void> {
  nonce = message.nonce;
  currentGeneration = message.context?.generation ?? 0;
  pocMode = message.context?.poc ?? false;
  try {
    installNetworkLockdown();
    installLxContract(message.context?.scriptInfo);
    Object.assign(globalThis, {
      ls: message.context?.config ?? { api: { addr: "http://gx.invalid/", pass: "" } },
    });
    await (0, eval)(message.script ?? "");
  } catch (error) {
    post({
      type: "runtimeFailure",
      generation: currentGeneration,
      stage: "community-script",
      error: String(error),
      poc: pocMode,
    });
  }
}

async function dispatch(message: ParentMessage): Promise<void> {
  try {
    if (!requestHandler) throw new Error("Request event is not defined");
    if (!message.poc && message.generation !== currentGeneration) {
      throw new Error("LX request generation does not match the active source realm");
    }
    const raw = await requestHandler(message.payload);
    const result = message.poc && typeof raw === "string" ? { url: raw, type: "128k" } : raw;
    post({
      type: "runtimeResult",
      requestId: message.requestId,
      generation: message.generation ?? currentGeneration,
      result,
      error: null,
      poc: Boolean(message.poc),
    });
  } catch (error) {
    post({
      type: "runtimeResult",
      requestId: message.requestId,
      generation: message.generation ?? currentGeneration,
      result: null,
      error: String(error),
      poc: Boolean(message.poc),
    });
  }
}

async function cryptoSelfTest(): Promise<void> {
  try {
    const lx = (globalThis as typeof globalThis & { lx: any }).lx;
    const publicKey = `-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDgtQn2JZ34ZC28NWYpAUd98iZ37BUrX/aKzmFbt7clFSs6sXqHauqKWqdtLkF2KexO40H1YTX8z2lSgBBOAxLsvaklV8k4cBFK9snQXE9/DDaFt6Rr7iVZMldczhC0JNgTz+SHXT6CBHuX3e9SdB1Ua44oncaTWz7OBGLbCiK45wIDAQAB\n-----END PUBLIC KEY-----`;
    const aes = lx.utils.crypto.aesEncrypt(
      Buffer.from("phase-1"),
      "aes-128-cbc",
      Buffer.from("0123456789abcdef"),
      Buffer.from("abcdef0123456789"),
    );
    const rsa = lx.utils.crypto.rsaEncrypt(Buffer.from("phase-1"), publicKey);
    const md5 = lx.utils.crypto.md5("phase-1");
    const random = lx.utils.crypto.randomBytes(32);
    const compressed = await lx.utils.zlib.deflate(Buffer.from("gxplayer-zlib"));
    const inflated = await lx.utils.zlib.inflate(compressed);
    const passed =
      Buffer.isBuffer(aes) &&
      aes.length === 16 &&
      Buffer.isBuffer(rsa) &&
      rsa.length === 128 &&
      md5 === "886fb78f4674a6619afd8822efc65877" &&
      random.length === 32 &&
      inflated.toString() === "gxplayer-zlib";
    post({
      type: "cryptoResult",
      passed,
      details: { aesLength: aes.length, rsaLength: rsa.length, md5 },
    });
  } catch (error) {
    post({ type: "cryptoResult", error: String(error) });
  }
}

self.addEventListener("message", (event: MessageEvent<ParentMessage>) => {
  const message = event.data;
  if (message.type === "launch") {
    void launch(message);
    return;
  }
  if (!nonce || message.nonce !== nonce) return;
  if (message.type === "bridgeResult") {
    if (typeof message.callId !== "number") return;
    const pending = pendingBridge.get(message.callId);
    if (!pending) return;
    pendingBridge.delete(message.callId);
    if (message.error) pending.reject(new Error(message.error));
    else pending.resolve(message.result);
  } else if (message.type === "dispatch") {
    void dispatch(message);
  } else if (message.type === "cryptoSelfTest") {
    void cryptoSelfTest();
  }
});
