# Cross-layer data contracts

The executable definitions live in `crates/gx-contracts` and are serialized with Serde.

## Track

`Track` contains display metadata and either a local path or an `OnlineRef`. `OnlineRef.resolver_payload` is deliberately opaque. LX-specific names and shapes are forbidden in the audio and library cores.

## ResolvedMediaRequest

Online resolution returns a structured request containing URL, ordered headers, media type, quality, and optional expiry. Logs must use the redacted diagnostic representation; query parameters and header values may contain credentials.

The first implementation supports progressive HTTP MP3/FLAC. HLS is represented in the contract but may return an explicit unsupported-media error in v1 until a separate HLS pipeline is implemented.

## Versioning

Contracts begin at application version 0.1. Breaking persisted-data changes require a migration. IPC consumers must ignore unknown additive fields where their serializer permits it.

