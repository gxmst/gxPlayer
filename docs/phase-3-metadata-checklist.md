# Phase 3 metadata acceptance

Status: passed on Windows 11, 2026-07-11.

## Delivered

- New provider-neutral `gx-metadata` core; no LX objects leak into catalog tracks, lyrics, replacement matching, or playback requests.
- Real search aggregates the public iTunes Search API and Deezer catalog API concurrently.
- Real China-region popular chart uses Apple's public Marketing Tools RSS JSON API.
- Real lyrics use LRCLIB with bounded HTTP, retries, duration-aware candidate selection, synced LRC parsing, multiple timestamps per line, and plain-lyrics fallback.
- Search results contain provider-owned opaque resolver payloads and optional structured `ResolvedMediaRequest` previews; no bare media URL crosses into the audio engine.
- Cross-platform replacement ranks title, artist, album, and duration, excludes the failed provider, probes candidate availability, and selects the best playable provider.
- Deezer remains a real metadata/replacement source, but its current preview MP3 payloads are not advertised as playable because Symphonia 0.5 rejects their frame layout. The fallback path selects a compatible iTunes preview.
- Official 30-second previews use the approved playback-cache scope, then enter the same native local decode/DSP/cpal engine. Full online songs continue to use Phase 2 progressive streaming; this cache path is not a download manager or whole-song pseudo-streaming path.
- AAC/M4A files whose container omits initial codec channel/rate fields are now probed by decoding and preserving the first packet, fixing both cached preview playback and local M4A playback.
- Development UI includes search, chart loading, result playback, automatic replacement notification, synchronized lyric highlighting, and smooth lyric auto-scroll.
- NetEase public share-playlist import remains intentionally unimplemented because v1.1 marks it optional and non-blocking.

## Automated verification

- `cargo test --workspace`
  - multi-timestamp LRC parsing and ordering
  - provider fixture mapping to structured media requests
  - replacement scoring
  - unavailable-original automatic fallback selection
  - all prior Phase -1 through Phase 2 regressions
- `cargo test -p gx-streaming --no-default-features`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `npm run build`

## Repeatable live-service verification

`cargo run -p metadata-smoke -- "Hello Adele"`:

- `GX_PHASE3_SEARCH_OK count=20 providers={"deezer", "itunes"}`
- `GX_PHASE3_STRUCTURED_PREVIEW_OK`
- `GX_PHASE3_LYRICS_OK lines=48`
- `GX_PHASE3_CHART_OK count=10`
- `GX_PHASE3_REPLACEMENT_OK matches=1`

The metadata client retries transient service/network failures three times with bounded backoff.

## Real-device end-to-end verification

The automated Tauri smoke deliberately starts from a Deezer catalog result whose preview is unavailable to the native decoder, then finds the matching iTunes result, caches its official 30-second M4A preview, fetches lyrics for the original standard track identity, and plays through the Realtek device:

- `GX_PHASE3_REPLACEMENT_OK from=deezer to=itunes`
- `GX_PHASE3_LYRICS_OK lines=45`
- `GX_PHASE3_SEARCH_PLAY_LYRICS_OK position=0.212 underruns=0`

Phase 2's unchanged community-script/native-streaming smoke was rerun after the audio probing change:

- `GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position=0.292 underruns=0`
- `GX_PHASE2_LX_E2E_OK`

The local engine smoke also passed pause, seek, volume, DSP, next-track, automatic progression, and zero-underrun checks.

## Phase 3 gate

The required path passes: real search -> results -> click/automatic platform replacement -> native audible playback -> timestamped lyric highlighting/scrolling. Real chart and lyric services are connected. The optional public-share-playlist import does not block Phase 4.
