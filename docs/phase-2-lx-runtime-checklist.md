# Phase 2 LX source runtime acceptance

Status: passed on Windows 11, 2026-07-11.

## Delivered

- Hidden `lx-sandbox` WebView is created in normal application operation.
- The sandbox keeps an empty Tauri capability and has no filesystem, shell, clipboard, dialog, opener, or main-window command permission.
- Navigation is restricted to the packaged/development `sandbox.html`; `window.open` is denied.
- The factual synchronous LX compatibility surface remains inside JavaScript: Buffer, AES, raw RSA with 128-byte left padding, MD5, random bytes, and zlib.
- Source storage supports URL import, local-file import, SHA-256 deduplication, activation, deletion, update-alert toggles, raw-script backup/restore, and atomic persistence.
- Runtime operations are serialized. Every launch has a generation and every resolver call has a correlated request ID. Reload, crash, timeout, and stale responses reject pending calls.
- A request may temporarily select a non-active source; the persisted active source is restored after success or failure.
- Only `action: "musicUrl"` crosses the source-runtime boundary.
- Resolver output is normalized to `ResolvedMediaRequest` with URL, ordered headers, media type, quality, and optional expiry. Core `Track`/audio types do not contain LX objects.
- Native progressive playback accepts `ResolvedMediaRequest`; online MP3 seeking uses coarse Range seek plus timestamp-delta PCM discard.

## HTTP and SSRF controls

- HTTP(S) only; URL credentials are denied.
- DNS is resolved and checked before connection; loopback, private, link-local, unspecified, broadcast, and documentation ranges are denied.
- The selected public address is pinned into reqwest to prevent DNS rebinding.
- Every redirect is resolved, checked, and pinned again.
- Authorization, Proxy-Authorization, and Cookie headers are stripped on cross-origin redirects.
- URL, options, request body, header count/value, response body, redirect count, connect timeout, request timeout, and IPC JSON sizes are bounded.
- The media streaming client independently repeats the public-destination and per-redirect pinning checks; a source script cannot bypass SSRF by returning a private media URL.

## Automated verification

- `cargo test --workspace`
  - source import/dedup/activation/removal/reopen
  - backup/restore and update-alert preference persistence
  - private/credential URL rejection
  - cross-origin credential stripping
  - oversized HTTP response rejection
  - runtime generation/request correlation
  - crash/reload pending rejection and stale-response rejection
  - temporary source launch leaves persisted active source unchanged
  - non-`musicUrl` action rejection
  - structured media normalization and redacted diagnostics
  - prior Phase 0/1 audio and DSP regression suites
- `cargo test -p gx-streaming --no-default-features`
  - production media policy rejects private destinations
- `cargo clippy --workspace --all-targets -- -D warnings`
- `npm run build`
- HTTP reconnect/Range/backpressure regression:
  - `cargo run -p http-stream -- --self-test <test.mp3> 30`
  - reconnect observed, Range seek observed, bounded-channel backpressure observed.

## Community-script and real-device verification

Unmodified community script:

- upstream commit: `da7759eb54a9e293b5594933ebff61043e8c46cd`
- script SHA-256: `59cbe534a2c8ef68bdac79cfcb2e798b4d6f150ae7a087d7020758d7f136fe7c`

Phase -1 sandbox regression markers:

- `GX_PHASE1_LX_MUSIC_URL_OK`
- `GX_PHASE1_LX_SYNC_CRYPTO_OK`
- `GX_PHASE1_LX_SECURITY_OK`
- `GX_PHASE1_LX_SANDBOX_OK`

Phase 2 end-to-end used the unchanged script, deterministic API responses, and the public SoundHelix example MP3. The result was normalized and passed to the Rust engine on the Realtek output device:

- `GX_PHASE2_LX_RESOLVED_OK`
- `GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position=0.212 underruns=0`
- `GX_PHASE2_LX_E2E_OK`

No copyrighted provider media was fetched for acceptance testing.

## Phase 2 gate

The v1.1 acceptance path passes: import source -> mocked search metadata/resolver payload -> community script returns a playable request -> structured native streaming -> audible real-device output. Sandbox file/shell/clipboard/opener/main-command access and private-network HTTP are denied. Phase 3 may begin.
