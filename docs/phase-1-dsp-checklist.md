# Phase 1 DSP verification checklist

- [x] DSP lives on the decode/processing worker before the PCM output ring.
- [x] The cpal callback remains a pure ring-buffer copy path with atomics only.
- [x] Callback-path allocation test records zero heap allocations.
- [x] Whole-chain bypass returns before validation or sample access.
- [x] Disabled-chain PCM compares bit-for-bit with input, including signed zero and NaN payload bits.
- [x] Enabled chain with EQ disabled is also bit-for-bit transparent.
- [x] Zero-dB peak/shelf filters use exact identity coefficients.
- [x] RBJ peak coefficients match an independent golden reference.
- [x] A 1 kHz, +6 dB peak filter measures within 0.08 dB of target after settling.
- [x] Aggressive valid multi-band parameters remain finite for an impulse response.
- [x] Invalid frequency/Q/gain and misaligned PCM are rejected.
- [x] DSP processing performs zero heap allocations after configuration.
- [x] Engine smoke test enables +9 dB EQ, seeks while enabled, then returns to full bypass.
- [x] Seek, queue switching, pause/resume, volume, and automatic next remain operational.
- [x] Main WebView DSP controls runtime marker observed after the UI change.
- [x] Full workspace tests, strict clippy, and frontend production build pass.

## Signal order in Phase 1

```text
Symphonia decode -> rubato rate adaptation -> DSP chain -> worker-side volume -> PCM ring -> cpal callback
```

The DSP and volume stages never run in the cpal callback. Settings changes recreate the session at its current timestamp so already-buffered PCM with old coefficients/gain cannot leak after a control change.
