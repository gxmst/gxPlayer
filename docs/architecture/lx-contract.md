# LX source runtime contract

The runtime targets the factual `window.lx` contract used by community LX source scripts and is checked against Sollin Music Desktop.

## Adapters live outside this repository

This repository implements the host side of the contract only. The host knows
nothing about any specific platform, endpoint, or quality table: every platform
name, quality label, and provider identifier reaches it at runtime through a
source's `inited` capability report.

Source scripts themselves are user-owned and are never committed here. `/sources/`
is git-ignored so an imported adapter and its local test can sit next to each
other on disk without entering version control. Do not add an adapter, its API
host, its quality mapping, or platform-specific field parsing to project code —
that would move the project from consuming the contract to publishing a
platform integration.

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

Optional actions never delay audio. They share one sandbox and one operation
lock with playback resolution, so the host holds that lock as briefly as it can:

- A source already loaded and ready is preferred, even when a higher-ordered
  source also advertises the action, because reloading the realm would cost two
  initializations while the user is listening.
- When a switch is unavoidable, the previous source is restored in the
  background instead of under the lock.
- Any playback request preempts the in-flight optional action, which observes
  the cancelled token within one poll interval and releases the lock. A
  preempted optional action reports no result rather than an error.

- `search` receives `{ action, info: { keyword, limit, offset } }` and returns
  either an array of normalized `CatalogTrack` objects or `{ tracks }`.
- `lyric` receives the selected platform and `musicInfo` and returns a
  normalized `LyricDocument` or `null`.
- `playlist` receives `{ action, info: { id } }` and returns playlist metadata
  plus normalized `CatalogTrack` objects. `source` is added only when the
  capability report named a platform for this action, and the id format is
  whatever that source accepts. The host has no platform table and never
  defaults to one.
- `musicUrl` keeps the existing LX payload and URL result contract.

The normalized contracts are size-limited and validated again in Rust. A
source may cache results or coalesce identical in-flight requests, but must not
hide additional provider fan-out behind one action. Unusable rows in a returned
page are skipped rather than failing the page; a page where nothing is usable is
reported as an error. A source should keep its own request timeout below the
host's 8s runtime budget so its transport errors stay visible.

Optional-action failures do not enter the source health window. That window
ranks sources for resolution, and a source whose optional action is rejected —
an exhausted search quota, say — can still resolve and play audio, so demoting
it would be wrong. These failures are written to the diagnostic log under
`source_optional_action_failed` instead.

## Security boundary

The sandbox window has a dedicated capability with no filesystem, shell, clipboard, opener, dialog, or main-window commands. Host HTTP accepts only HTTP(S), rejects credentials in URLs, limits redirects/body size/time, and denies loopback, link-local, and private-network destinations by default. Every IPC message is size-limited and accepted only from the sandbox label.
