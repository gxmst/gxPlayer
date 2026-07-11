# GXPlayer

Windows-only desktop music player with a Rust-native playback/DSP pipeline and a Tauri UI.

The audio stream never passes through WebView or Web Audio. Local playback is the independent core; online metadata, LX source scripts, preview caching, and optional cinema/game spatial processing are separate layers.

## Current status

Phase -1 through Phase 5 are implemented:

- Local Symphonia decode, accurate local seek, rubato rate adaptation, and cpal output verified.
- Progressive HTTP decode, redirect handling, bounded backpressure, reconnect with Range, and online seek verified.
- Hidden Tauri LX sandbox verified with an unchanged community script, synchronous crypto/RSA no-padding, minimal capability, and SSRF/privilege rejection.
- Thread, state-machine, data, and LX contracts are recorded in `docs/architecture`.
- The default music mode is transparent DSP bypass. The retained Crossfeed + stereo HRTF + linked-limiter chain is an optional cinema/game mode after the user's blind test preferred bypass for music.
- SQLite persists the local library, favorites, playlists, and backup data. Local tags and durations are read during import.
- The Tauri UI follows the GXPlayer visual specification: custom title bar, collapsible navigation, persistent player, grouped debounced search, discovery, synchronized lyrics, source management, dynamic accent lighting, and an accessible mode-based sound-stage dial.
- Phase acceptance details are in `docs/phase-*-checklist.md`.

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
