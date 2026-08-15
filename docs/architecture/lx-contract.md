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
- `lx.on('request', handler)` registers one request handler. Protocol v1 sources
  receive `musicUrl` only. Protocol v2 sources may also receive `search`,
  `lyric` and `playlist` after advertising those actions during initialization.
- zlib inflate/deflate return Promises.

The bounded `inited` payload is also the sole source of capability labels shown
in the source manager. GXPlayer caches each source's reported `sources` keys and
its `qualitys`/`qualities` strings after that source initializes. It never scans
or evaluates script text to infer support, and an absent or malformed report is
displayed as unavailable rather than guessed.

## Protocol v2 optional actions

A v2 source opts in by including both fields in its `inited` payload:

```json
{
  "protocolVersion": 2,
  "actions": ["musicUrl", "search", "lyric", "playlist"]
}
```

The host never sends an optional action to a source that did not advertise it.
Optional actions are budgeted: one user operation selects one enabled source
and one network route. The host does not fan a search out across imported
sources. Search suggestions remain on the built-in metadata path; `search` is
used only when a completed built-in search returned no tracks.

- `search` receives `{ action, info: { keyword, limit, offset } }` and returns
  either an array of normalized `CatalogTrack` objects or `{ tracks }`.
- `lyric` receives the selected platform and `musicInfo` and returns a
  normalized `LyricDocument` or `null`.
- `playlist` receives `{ action, source, info: { id } }` and returns playlist
  metadata plus normalized `CatalogTrack` objects.
- `musicUrl` keeps the existing LX payload and URL result contract.

The normalized contracts are size-limited and validated again in Rust. A
source may cache results or coalesce identical in-flight requests, but must not
hide additional provider fan-out behind one action.

## Security boundary

The sandbox window has a dedicated capability with no filesystem, shell, clipboard, opener, dialog, or main-window commands. Host HTTP accepts only HTTP(S), rejects credentials in URLs, limits redirects/body size/time, and denies loopback, link-local, and private-network destinations by default. Every IPC message is size-limited and accepted only from the sandbox label.
