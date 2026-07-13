# LX source runtime contract

The runtime targets the factual `window.lx` contract used by community LX source scripts and is checked against Sollin Music Desktop.

## Sandbox-only synchronous APIs

- `lx.utils.crypto.aesEncrypt`
- `lx.utils.crypto.rsaEncrypt` using RSA no-padding with manual 128-byte left padding
- `lx.utils.crypto.randomBytes`
- `lx.utils.crypto.md5`
- `lx.utils.buffer.from` and `bufToString`

These APIs must return synchronously inside the sandbox JavaScript context. Tauri `invoke` is not permitted for them.

## Asynchronous host bridge

- `lx.request(url, options, callback)` delegates sanitized HTTP to Rust and returns a cancel function.
- `lx.send(...)` returns a Promise and exposes only `inited` and `updateAlert`.
- `lx.on('request', handler)` registers the single `musicUrl` request handler.
- zlib inflate/deflate return Promises.

The bounded `inited` payload is also the sole source of capability labels shown
in the source manager. GXPlayer caches each source's reported `sources` keys and
its `qualitys`/`qualities` strings after that source initializes. It never scans
or evaluates script text to infer support, and an absent or malformed report is
displayed as unavailable rather than guessed.

## Security boundary

The sandbox window has a dedicated capability with no filesystem, shell, clipboard, opener, dialog, or main-window commands. Host HTTP accepts only HTTP(S), rejects credentials in URLs, limits redirects/body size/time, and denies loopback, link-local, and private-network destinations by default. Every IPC message is size-limited and accepted only from the sandbox label.
