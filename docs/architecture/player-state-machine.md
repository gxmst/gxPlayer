# Player state machine

The authoritative states are `idle`, `loading`, `playing`, `paused`, `buffering`, `stopped`, and `failed`.

```text
idle/stopped --load--> loading --ready+play--> playing
loading --ready+no-autoplay--> paused
playing --pause--> paused --resume--> playing
playing --starved--> buffering --data-ready--> playing
paused/playing/buffering --seek--> loading --ready--> previous intent
any active state --stop--> stopped
loading/playing/paused/buffering --fatal error--> failed
failed --load--> loading
```

Rules:

- `paused` retains the current track and position; `stopped` resets position.
- A seek preserves play/pause intent and increments the generation.
- Loading a new track increments the generation and invalidates all older asynchronous events.
- Recoverable network starvation is `buffering`; decoder corruption, unsupported media, and exhausted retries are `failed`.
- UI state is a projection of `PlaybackSnapshot`; the UI never invents a playback transition locally.

