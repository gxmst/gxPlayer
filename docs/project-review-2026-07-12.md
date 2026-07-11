# GXPlayer project review — 2026-07-12

## Executive assessment

The native audio, sandbox boundary, metadata services, and local persistence are useful foundations, but the project was declared daily-use ready too early. This review found one release-blocking audio scheduling bug, two visible window/layout defects, and an important gap between the LX source runtime and the normal search/play user flow.

The audio and window/layout defects are fixed in the accompanying change. Online full-track source integration remains incomplete and must be treated as the next product milestone rather than hidden behind the working preview path.

## Fixed release blockers

### P0 — Resampled AAC playback starved the output ring

The worker waited four milliseconds after every successful decode/resample step as well as after genuine ring backpressure. On the user's real 44.1 kHz iTunes M4A preview with a 48 kHz Realtek shared-mode output, production fell behind consumption.

Before the fix:

```text
GX_ENGINE_STABILITY_OK position=19.167 underruns=1015 output_rate=Some(48000)
```

after 25 seconds of wall time.

The worker now waits only when the ring is actually backpressured and immediately continues after useful decode/pump progress.

After the fix:

```text
GX_ENGINE_STABILITY_OK position=25.022 underruns=0 output_rate=Some(48000)
```

The engine snapshot now reports the active output sample rate, and the smoke tool has a reusable long-form stability mode. A 30-second 44.1-to-48 kHz duration-preservation unit test was also added.

### P1 — Default window was too small and not centered

The packaged configuration opened at 800x600 logical pixels even though the main layout was designed around a much wider desktop canvas. The window now starts centered and chooses a 16:10 logical size from 88% of the active monitor, capped at 1280 pixels wide and 86% of monitor height. The static fallback is 1100x688.

On the current monitor the verified result is centered at 1295x809 physical pixels inside a 2293x912 work area.

### P1 — Mid-width layout collisions

The full 230-pixel sidebar remained active until 980 pixels while the now-playing page contained nested minimum-width grids. At approximately 1100 pixels this pushed the mode controls outside the visible content area. The compact sidebar breakpoint is now 1200 pixels, the player bar minimum columns are smaller, page bottom padding clears the persistent player, and the now-playing layout only stacks below 900 pixels.

Visual checks were repeated for discovery, settings, and now-playing at 1100x688 plus now-playing at 800x600.

## LX / third-party source audit

No usable third-party playback source is bundled with GXPlayer.

- Sollin's open-source repository explicitly states that it contains no private, paid, or built-in LX source scripts.
- Modern LX Music also removed its built-in playable sources; it expects user-imported community scripts.
- GXPlayer implemented and tested the compatibility runtime, storage, sandbox, and import flows.
- The only script present in normal GXPlayer AppData was the Phase 2 deterministic mock fixture. Without the mock service it timed out during initialization and was not a production source.
- That test fixture has been removed from normal AppData. Automated smoke runs now use a process-isolated temporary data root so tests cannot alter the user's library, source list, or preview cache again.

Bundling arbitrary community playback scripts would contradict the architecture brief's explicit rule that the program itself does not ship a playable source, and would introduce security, licensing, reliability, and service-policy risks. A future curated directory may list user-installable source URLs and provenance, but shipping or recommending entries requires an explicit product/legal decision.

## Remaining product gaps

### P1 — Normal online play does not use the LX source runtime

The current global search uses iTunes and Deezer metadata. Clicking a result plays an official 30-second iTunes preview through the playback cache. The normal UI does not yet translate a catalog result into the platform-specific `musicInfo` payload required by an LX script and call `source_resolve` for full-track playback.

This is the largest remaining gap between “source runtime works in a PoC” and “online listening works as a daily product.” It needs a provider-neutral catalog-to-LX adapter, platform identity mapping, quality selection, source fallback, and a real non-mock community-script acceptance run.

### P1 — UI code is too monolithic

`App.tsx` owns navigation, search, source management, library, playback, lyrics, backup, and window placement. `App.css` also contains the entire visual system. This is workable for a prototype but raises regression risk. Split the UI by page and domain hooks before adding more features.

### P1 — Local library workflow is still basic

The local library supports selected-file import, tags, favorites, playlists, and backup, but lacks folder scanning/watching, missing-file reconciliation, embedded-cover extraction, and queue-from-current-view behavior. Clicking a library row currently loads only that one track rather than the surrounding list.

### P2 — Window preference is not persisted

The proportional centered size is reapplied on every launch. A later window-state setting should remember a user's last non-maximized size and position while retaining the safe centered default for first launch or invalid/off-screen coordinates.

### P2 — Artwork color extraction may fall back

Canvas-based cover sampling depends on remote image CORS. When unavailable, GXPlayer uses a deterministic accessible color derived from track identity. A native/cache-backed artwork pipeline would make cover extraction and offline presentation more reliable.

### P2 — Frontend automated coverage is thin

Rust behavior has strong automated coverage, but the React UI currently relies on TypeScript builds and rendered screenshot review. Add component tests for search keyboard behavior, mode switching, library actions, and responsive layout states.

## Strengths worth preserving

- Audio never crosses the WebView boundary.
- DSP bypass and callback real-time constraints are covered by tests.
- Structured media requests preserve headers, expiry, and media type.
- HTTP and media clients independently enforce SSRF and redirect controls.
- The LX runtime is isolated in a separate minimal-capability WebView.
- Local library data is SQLite-backed and backupable.
- Product wording no longer promises shared-mode device bit-perfect output.
