# Phase 4 spatial audio acceptance

Status: accepted with an architect-authorized product-positioning adjustment on 2026-07-12.

The objective gate passed. The user then completed the blind test and preferred the bypass presentation, so the spatial chain is retained as an optional cinema/game mode rather than treated as the default music path. The architect explicitly waived the former subjective-pass requirement and authorized Phase 5 to begin.

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

## Subjective result and product decision

A 12-trial randomized A/B package was generated at:

```text
C:\Users\super\AppData\Local\Temp\gxplayer-spatial-blind-phase4
```

It contains dry/spatial references, randomized trial WAV files, `trials.json`, hidden scoring answers, and `listening-notes.md`.

The user's 12 responses were:

```text
ABAAABAAAABB
```

Scoring marker:

```text
GX_PHASE4_BLIND_SCORE correct=11 total=12 accuracy=91.7%
```

The user could reliably distinguish the processing but reported that bypass had the better soundstage and that the spatial version did not improve music listening. This is treated as a useful negative product result, not as a failed technical implementation.

Final positioning:

- Default `music` mode is transparent DSP bypass.
- Optional `cinema_game` mode enables Crossfeed, stereo HRTF, and the linked limiter.
- Parametric EQ is not exposed as a product feature.
- The mode interface remains extensible for future switch-style processing modes.

Phase 4 is accepted under this revised positioning. Phase 5 may proceed.
