# GXPlayer

Windows-only desktop music player with a Rust-native playback/DSP pipeline and a Tauri UI.

The audio stream never passes through WebView or Web Audio. Local playback is the independent core; online metadata, LX source scripts, playback caching, EQ, crossfeed, and stereo HRTF are optional layers.

## Current status

Phase -1 is complete:

- Local Symphonia decode, accurate local seek, rubato rate adaptation, and cpal output verified.
- Progressive HTTP decode, redirect handling, bounded backpressure, reconnect with Range, and online seek verified.
- Hidden Tauri LX sandbox verified with an unchanged community script, synchronous crypto/RSA no-padding, minimal capability, and SSRF/privilege rejection.
- Thread, state-machine, data, and LX contracts are recorded in `docs/architecture`.

Phase 0 is complete: the persistent headless local playback engine and deliberately minimal development UI support file selection, play/pause, accurate local seek, worker-side volume, previous/next, and automatic queue progression. Phase 1 is next: a transparent-bypass DSP frame and parametric EQ.

## Development

Requirements: Windows, WebView2, Rust stable (MSVC), Node.js, and npm.

```powershell
npm install
npm run build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run tauri dev
```

The legacy WPF projects and large third-party datasets remain outside this repository and are read-only references.
