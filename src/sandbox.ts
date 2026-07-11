import { invoke } from "@tauri-apps/api/core";
import { Buffer } from "buffer";
import CryptoJS from "crypto-js";
import forge from "node-forge";
import * as pako from "pako";

type RequestHandler = (payload: unknown) => unknown | Promise<unknown>;

type LxHttpResponse = {
  statusCode: number;
  headers: Record<string, string>;
  body: unknown;
};

let requestHandler: RequestHandler | null = null;

const EVENT_NAMES = {
  request: "request",
  inited: "inited",
  updateAlert: "updateAlert",
} as const;

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

function installLxContract(): void {
  Object.assign(globalThis, { Buffer });
  Object.assign(globalThis, {
    lx: {
      EVENT_NAMES,
      version: "2.0.0",
      env: "desktop",
      currentScriptInfo: {
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
        void invoke<LxHttpResponse>("lx_http_request", { url, options })
          .then((response) => {
            if (!cancelled) callback(null, response, response.body);
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
        return invoke("lx_send", { eventName, data });
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
    },
  });
}

Object.assign(window, {
  async __gxRunCommunityScript(script: string) {
    try {
      installLxContract();
      Object.assign(globalThis, {
        ls: { api: { addr: "http://gx.invalid/", pass: "" } },
      });
      await (0, eval)(script);
    } catch (error) {
      await invoke("lx_poc_failure", { stage: "community-script", error: String(error) });
    }
  },
  async __gxDispatchRequest(payload: unknown) {
    try {
      if (!requestHandler) throw new Error("Request event is not defined");
      const raw = await requestHandler(payload);
      const result = typeof raw === "string" ? { url: raw, type: "128k" } : raw;
      await invoke("lx_poc_result", { result });
    } catch (error) {
      await invoke("lx_poc_failure", { stage: "music-url", error: String(error) });
    }
  },
  async __gxRunCryptoSelfTest() {
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
      await invoke("lx_crypto_result", {
        passed,
        details: { aesLength: aes.length, rsaLength: rsa.length, md5 },
      });
    } catch (error) {
      await invoke("lx_poc_failure", { stage: "sync-crypto", error: String(error) });
    }
  },
  async __gxRunSecuritySelfTest() {
    const results = { mainCommandBlocked: false, openerBlocked: false, ssrfBlocked: false };
    try {
      await invoke("main_only_probe");
    } catch {
      results.mainCommandBlocked = true;
    }
    try {
      await invoke("plugin:opener|open_url", { url: "https://example.com" });
    } catch {
      results.openerBlocked = true;
    }
    try {
      await invoke("lx_http_request", { url: "http://127.0.0.1/private", options: {} });
    } catch {
      results.ssrfBlocked = true;
    }
    await invoke("lx_security_result", { results });
  },
});

void invoke("sandbox_ready").catch((error) => {
  console.error("Failed to announce LX sandbox readiness", error);
});
