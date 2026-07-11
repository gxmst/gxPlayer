# Phase 4 spatial audio acceptance

Status: objective checks passed on Windows 11, 2026-07-11; final subjective gate is pending.

Phase 5 must not begin until a human completes the blind identification and at least 30 minutes of listening notes required by the v1.1 architecture brief.

## Delivered

- DSP order is `EQ -> Crossfeed -> stereo HRTF -> linked limiter`; the master bypass still returns before reading or writing PCM.
- Crossfeed provides bounded amount, interaural delay, and low-pass cutoff controls with preallocated delay storage.
- Stereo HRTF uses four uniform-partitioned convolution paths for L/R to near/far ears and a fixed 128-frame algorithmic latency.
- Embedded compact HRIR data is the MIT Media Lab KEMAR elevation 0 degree, azimuth 30 degree measurement at 44.1 kHz, with runtime resampling for 48 and 96 kHz.
- A linked-stereo limiter preserves channel balance while enforcing the configured ceiling.
- The development UI exposes Crossfeed, HRTF mix, limiter linkage, an honest generic-HRTF limitation warning, and output-device selection.
- The audio engine enumerates named output devices, switches devices from the current playback position, reports unavailable devices clearly, and can recover to the prior/default device.
- The Windows shared-mode startup buffer is 500 ms. Ring occupancy now comes from the ring buffer itself rather than a separately updated approximate counter, removing a producer/callback race observed during real-device spatial playback.
- KEMAR attribution and redistribution terms are recorded in `third_party/licenses/MIT-KEMAR.txt`.

## Automated verification

- `cargo test --workspace`
  - 17 `gx-dsp` tests cover PCM bitwise master/EQ bypass, RBJ EQ references, direct-vs-partitioned convolution, embedded KEMAR impulse and 250/1000/8000 Hz golden responses, interaural timing/energy, Crossfeed response, linked limiter ceiling, chunk invariance, invalid settings, finite output, 44.1/48/96 kHz, debug CPU budget, and zero processing allocations.
  - Audio callback tests verify zero allocation and atomic-only callback bookkeeping.
  - All Phase -1 through Phase 3 regression tests pass.
- `cargo test -p gx-streaming --no-default-features`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `npm run build`

## Objective measurement

`cargo run -p spatial-lab -- measure`:

```text
GX_PHASE4_MEASURE_OK latency_frames=128 latency_ms=2.667 peak=0.152622 cpu_realtime_ratio=0.1353
```

## Real-device verification

`cargo run -p engine-smoke -- "E:\diff\gxMusic\gxPlayer\GxPlayer\Assets\test.mp3"` passed on two enumerated devices:

- `CS3555 (NVIDIA High Definition Audio)`
- `扬声器 (Realtek High Definition Audio)`

Verified playback, pause, seek, volume, next/previous, EQ, Crossfeed + HRTF + limiter, spatial seek, two-second stability with zero underruns, device switch, unavailable-device failure, device recovery, bypass restore, and automatic next-track progression.

Phase 2 and Phase 3 real-service/device regressions after the audio-buffer fix:

```text
GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position=0.272 underruns=0
GX_PHASE2_LX_E2E_OK
GX_PHASE3_REPLACEMENT_OK from=deezer to=itunes
GX_PHASE3_LYRICS_OK lines=45
GX_PHASE3_SEARCH_PLAY_LYRICS_OK position=0.242 underruns=0
```

## Pending subjective gate

A 12-trial randomized A/B package was generated at:

```text
C:\Users\super\AppData\Local\Temp\gxplayer-spatial-blind-phase4
```

It contains dry/spatial references, randomized trial WAV files, `trials.json`, hidden scoring answers, and `listening-notes.md`.

Required human steps:

1. Listen on headphones at a safe, fixed volume without opening `answers.json`.
2. Record 12 A/B identifications in a JSON string array, then run `cargo run -p spatial-lab -- score <directory> <responses.json>`.
3. Listen for at least 30 minutes and complete the supplied notes for front/back confusion, externalization, coloration, fatigue, and preferred mix.
4. Decide whether the experience is acceptable or requires HRTF/mix tuning, then record the decision here.

Until those steps are complete, Phase 4 is not accepted and Phase 5 is blocked by the architecture brief's self-check marathon rule.
