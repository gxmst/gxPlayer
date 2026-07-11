# Thread model

This document is a Phase -1 contract. Changes require an architecture review.

## Execution contexts

1. **Main WebView/UI thread** renders state and emits commands. It never owns playback.
2. **Audio device callback** only copies already-prepared PCM from a bounded SPSC ring buffer into the device buffer and updates lock-free counters. It performs no allocation, locking, file I/O, network I/O, decoding, resampling, or DSP.
3. **Decode/DSP worker** owns demuxer and decoder state, performs optional resampling and DSP, and writes prepared PCM into the output ring buffer.
4. **Network worker/runtime** resolves online tracks and fills a bounded byte buffer. Backpressure stops network reads before memory grows without bound.
5. **LX sandbox WebView** executes untrusted source scripts. It can access only the dedicated source-runtime IPC surface.

## Communication rules

- UI/control commands use bounded message channels.
- Network bytes and output PCM use bounded single-producer/single-consumer buffers.
- The audio callback never waits. On underrun it writes silence and increments a counter.
- Locks are allowed in cold control paths, but never in the audio callback or around a buffer consumed by it.
- Every track change increments a generation number. Late network, decoder, and UI events from older generations are discarded.

## Shutdown order

Stop accepting commands, cancel network work, stop decode/DSP work, stop the device stream, then release buffers and state. No worker may outlive the owning engine.

