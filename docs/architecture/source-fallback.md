# Source fallback and cancellation contract

GXPlayer resolves only the track that is about to play. Queueing online tracks
must never resolve URLs in advance.

## Resolution order

1. Check completed local cache entries for the requested track and acceptable
   quality tiers.
2. Try the active LX source as the primary source.
3. If source fallback is enabled, try imported fallback sources in the persisted
   user order. The active source remains primary even if it also appears in the
   stored order.
4. Within each source, try the requested quality followed by lower advertised
   quality tiers.
5. For a direct user action only, the frontend may request an official preview
   after every full-track source attempt failed. Automatic failure skipping must
   not start a cascade of preview requests.

Every attempt records a bounded, credential-free diagnostic containing source,
provider, quality, stage, success, and a public error classification.

An explicit fallback order is opt-in. While the order is automatic, newly
imported sources follow stable import order; once the user saves an order, new
sources do not enter it until selected. A fallback source is launched in a
temporary generation and the persisted primary source is restored immediately
after the attempt without holding the resolver lock through a second 15-second
initialization wait.

## Request lifetime

Each play request has a unique request ID. Starting a newer request marks the
previous request stale. User cancellation marks it cancelled. A cancelled or
stale request may finish background cleanup, but it must never load or replace
the audio engine session and must never trigger automatic queue advancement.

The backend is authoritative for cancellation. A frontend timeout is only a UI
deadline and must also call the backend cancellation command.

The cancellation token is held while the final engine-load and media-metadata
commit is submitted. A stale token therefore cannot replace a newer session in
the small gap between checking cancellation and sending the engine command.

## Source runtime isolation

Switching, reloading, or temporarily trying a source must start it in a fresh JS
execution realm. Timers, globals, callbacks, and network work from the previous
realm must be terminated before the new source receives configuration or a
request. The production Worker is inlined as a Blob so it inherits the sandbox
CSP; `fetch`, WebSocket, XHR, nested workers, and `importScripts` are locked
before community code runs. The only network path exposed to a source is
`lx.request` through the Rust SSRF-checked bridge.
