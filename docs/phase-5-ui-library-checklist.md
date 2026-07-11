# Phase 5 UI, library, and product-mode acceptance

Status: passed on Windows 11, 2026-07-12.

## Product direction implemented

- Default `music` mode is transparent DSP bypass. The UI describes this honestly as engine-level transparent passthrough rather than claiming shared-mode device bit-perfect output.
- Optional `cinema_game` mode enables Crossfeed, stereo HRTF, and the linked limiter.
- Parametric EQ is no longer exposed as a product feature. Its tested low-level implementation remains available for compatibility and engineering regression coverage.
- `AudioMode` is a serialized engine contract, so future switch-style processing modes can be added without rebuilding the UI around individual DSP bands.

## Local daily-use features

- New `gx-library` crate backed by bundled SQLite.
- Local import reads duration and standard title, artist, and album tags, with filename fallback.
- Persistent local library, favorites, playlists, playlist membership/order, deletion, and combined library/source backup restore.
- Lists above 120 tracks use fixed-row windowed rendering instead of mounting the entire library in the WebView.
- Importing local files adds or refreshes their library records and starts native playback.

## UI specification coverage

- Tokenized near-black palette, elevated surfaces, glass borders, locally bundled Space Grotesk / Noto Sans SC / JetBrains Mono, and no runtime font dependency.
- Custom Tauri title bar with minimize, maximize, and close controls.
- Collapsible sidebar, discovery, search, local library, favorites, playlists, source management, settings, and persistent native player bar.
- Global search uses 200 ms debounce, stale-request cancellation, song/artist/album groups, keyboard selection, Enter, and Escape behavior.
- Discovery includes recent local music, playlists, and the live China-region chart.
- Playing an online catalog result performs replacement when required, caches only the official preview, fetches synchronized lyrics, and opens the immersive playback page.
- Dynamic accent extraction attempts artwork sampling with a deterministic accessible fallback color. Registered CSS color properties provide smooth accent/environment-light transitions.
- The playback page provides the record presentation, synchronized lyric focus, and the signature sound-stage visualization.
- The sound-stage control is mode-based after the Phase 4 decision. Both modes are native buttons/radio controls with visible focus and full keyboard access; there is no drag-only operation.
- Responsive checks were rendered at 1440x900 and 800x600. The small layout collapses sidebar text and preserves transport, search, and window controls.
- `prefers-reduced-motion` disables record rotation, panel motion, orbit animation, and long transitions.

## Automated and real-device verification

- `cargo test --workspace`
- `cargo test -p gx-streaming --no-default-features`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `npm run build`
- `npm run tauri build`
- `git diff --check`
- Real-device engine smoke passed with two enumerated devices, default `Music`, switch to `CinemaGame`, spatial seek/stability at zero underruns, invalid-device rejection, device recovery, and return to `Music` bypass.
- Phase 2 regression: `GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position=0.262 underruns=0`, followed by `GX_PHASE2_LX_E2E_OK`.
- Phase 3 regression: cross-provider replacement and 45 lyric lines, followed by `GX_PHASE3_SEARCH_PLAY_LYRICS_OK position=0.292 underruns=0`.

Release artifacts:

- `target/release/bundle/msi/gxplayer_0.1.0_x64_en-US.msi`
- `target/release/bundle/nsis/gxplayer_0.1.0_x64-setup.exe`

## Scope notes

- NetEase public share-playlist import remains optional and is not implemented.
- No account login or download manager was added.
- WASAPI exclusive device-level bit-perfect output remains a later enhancement; the current default guarantee is the previously tested PCM-boundary transparent bypass.
