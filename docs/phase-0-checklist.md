# Phase 0 verification checklist

- [x] Persistent Rust-owned local playback engine exists independently of UI/network/LX/DSP.
- [x] Symphonia decodes local MP3/WAV and the output worker adapts sample rate with rubato before cpal.
- [x] The cpal callback only consumes prepared PCM and updates atomics; it takes no locks and allocates nothing.
- [x] Prebuffering prevents the startup underrun observed in Phase -1.
- [x] Play, pause, resume, accurate local seek, volume, previous, next, and automatic queue progression pass the real-device smoke test.
- [x] Pause keeps the playback position stable.
- [x] The minimal Tauri development shell builds and exposes file selection, transport, seek, volume, queue, status, and diagnostics.
- [x] Playback state remains in Rust and the WebView only polls a serialized snapshot.
- [x] `decode_window` remains available as the PCM-boundary comparison hook for Phase 1.
- [x] Main WebView runtime smoke marker observed.
- [x] Full workspace tests, clippy with warnings denied, and frontend production build pass.

## Real-device evidence

- Device: Realtek High Definition Audio default output.
- Source: legacy read-only MP3, 44.1 kHz stereo; device output 48 kHz stereo.
- Startup underruns after prebuffering: 0.
- Pause position remained stable for 400 ms.
- Accurate local seek to 30.000 seconds succeeded while paused, then resumed.
- Volume changed to 25% on the worker processing path.
- Explicit next/previous transitions incremented the generation and recreated the native session.
- Two generated 0.4-second WAV queue entries advanced automatically from index 0 to index 1.
