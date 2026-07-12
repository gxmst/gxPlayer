# Phase -1 verification checklist

- [x] Independent repository created outside the legacy project.
- [x] Git initialized.
- [x] Clean React/Tauri template builds.
- [x] Rust workspace and executable contracts created.
- [x] Local cpal + Symphonia playback and seek PoC runs.
- [x] Progressive HTTP decoding, bounded buffering, redirects, recovery, and Range seek PoC runs.
- [x] Hidden WebView LX contract, synchronous crypto, and RSA no-padding PoC runs.
- [x] Sandbox capability and SSRF rejection tests pass.
- [x] Phase -1 evidence recorded; only then may Phase 0 begin.

## Evidence log

### Local playback PoC

- Input: legacy read-only `test.mp3`, MP3 stereo, 44.1 kHz.
- Seek: decoded a verification window beginning at 30.000 seconds.
- Device: Realtek High Definition Audio default output, 48 kHz stereo.
- Rate adaptation: rubato FFT resampling on the worker path, 44.1 kHz to 48 kHz.
- Playback: 132,300 source frames (3 seconds) completed.
- Observation: one startup underrun callback occurred before the buffer filled. Phase 0 must prebuffer before starting the device stream and track underruns as a quality metric.

### HTTP streaming PoC

- Input: the same MP3 served by a controlled local HTTP server.
- Redirect: `/redirect` resolved to `/media` and the final URL was retained for retries/seeks.
- Recovery: the server closed the first full response after 16 KiB; the worker reconnected with `Range` and decoding continued (`requests=2`, `reconnects=1`).
- Seek: a 300-second seek triggered a byte-range restart, then discarded 1,584 decoded frames to align the coarse byte seek with the requested timestamp.
- Backpressure: the bounded 8 × 64 KiB channel recorded blocked-send waits when the decoder intentionally stopped reading.
- Critical finding: Symphonia MP3 `SeekMode::Accurate` can linearly parse the stream. Online MP3 uses `SeekMode::Coarse` plus PCM timestamp discard; local files may continue to use accurate seek.

### LX sandbox PoC

- Community fixture: ZxwyWebSite/lx-script at revision `da7759eb54a9e293b5594933ebff61043e8c46cd`, executed unchanged from the ignored Phase-1 cache.
- The script initialized through the factual callback-style `lx.request`, registered `lx.on('request')`, sent `inited`, and returned `https://media.example/phase-1.mp3` for a deterministic mock request.
- Synchronous contract: AES returned 16 bytes, RSA no-padding returned 128 bytes, MD5 matched the platform reference, random bytes returned the requested length, and zlib round-tripped.
- Isolation: the sandbox has an empty dedicated Tauri capability and a restrictive CSP. Window-label authorization rejected a main-only command, the unavailable opener command was denied, and the HTTP bridge rejected loopback SSRF.
- The companion music-resolution service was not run and no music content was fetched.
