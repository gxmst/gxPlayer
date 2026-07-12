# GXPlayer

Windows-only desktop music player with a Rust-native playback/DSP pipeline and a Tauri UI.

The audio stream never passes through WebView or Web Audio. Local playback is the independent core; online metadata, LX source scripts, preview caching, and optional cinema/game spatial processing are separate layers.

## Current status

Phase -1 through Phase 5 are implemented:

- Local Symphonia decode, accurate local seek, rubato rate adaptation, and cpal output verified.
- Progressive HTTP decode, redirect handling, bounded backpressure, reconnect with Range, and online seek verified.
- Hidden Tauri LX sandbox verified with an unchanged community script, synchronous crypto/RSA no-padding, minimal capability, and SSRF/privilege rejection. Each source generation runs in a fresh inline Worker with direct network primitives disabled; source HTTP goes through the bounded Rust bridge.
- Online search can now carry native Kugou/Kuwo/NetEase LX metadata through `musicUrl` resolution into a structured media request and Rust-native progressive playback. Preview-sized resolver results are rejected instead of being reported as full tracks.
- Online resolution is request-scoped and cancellable. It tries the active source, the user-ordered fallback sources, and lower quality tiers before offering an official preview for a direct user action. Cache writes publish only after clean EOF.
- Windows System Media Transport Controls publish the current title, artist, album, artwork, duration, position, and play state to the taskbar media flyout and media keys. Play/Pause/Seek are native; Next/Previous use the mixed queue in the UI.
- Local import probes files once into SQLite and reports per-file failures. Playback and enqueue only send already-imported paths to the native engine; queue reorder preserves the current session and position.
- Thread, state-machine, data, and LX contracts are recorded in `docs/architecture`.
- The default music mode is transparent DSP bypass. The retained Crossfeed + stereo HRTF + linked-limiter chain is an optional cinema/game mode after the user's blind test preferred bypass for music.
- SQLite persists the local library, favorites, playlists, history, and backup data. Local tags and durations are read during import, with asynchronous batch import and missing-file reconciliation.
- The Tauri UI follows the GXPlayer visual specification: custom title bar, collapsible navigation, persistent player, grouped debounced search, discovery, synchronized lyrics, source management, dynamic accent lighting, and an accessible mode-based sound-stage dial.
- Phase acceptance details are in `docs/phase-*-checklist.md`.

## Development

Requirements: Windows, WebView2, Rust stable (MSVC), Node.js, and npm.

```powershell
npm ci
npm run test:unit
npm run build
node scripts/check-version.mjs
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run tauri dev
```

The legacy WPF projects and large third-party datasets remain outside this repository and are read-only references.

## User-provided LX sources

GXPlayer never bundles community source scripts. On startup it scans this repository-external directory and imports every valid `.js` file through the same validation and sandbox path as manual imports:

```text
%APPDATA%\com.gxplayer.desktop\sources\drop-in
```

Missing directories and individual invalid scripts do not prevent startup. The existing active source is preserved; otherwise the first valid drop-in source becomes active.

Fallback order is configured in the Sources view. In automatic mode, newly imported
sources follow stable import order; after the user saves an explicit order, new
sources remain opt-in. A failed full-track resolution never silently starts a
preview while advancing the queue.
