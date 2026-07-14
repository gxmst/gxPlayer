# Third-party provenance

GXPlayer vendors the MIT KEMAR compact HRIR dataset recorded in `third_party/licenses/MIT-KEMAR.txt` and bundles the fonts listed below. Rust and npm dependency provenance is locked by `Cargo.lock` and `package-lock.json`.

Planned references:

- LX Music Desktop, Apache-2.0, used as a metadata implementation reference.
- Sollin Music Desktop, MIT, used as the factual LX source-runtime compatibility reference.

Any future copied or adapted code must record upstream repository, revision, original path, local path, license, and update procedure before merge.

Phase -1 external compatibility fixture (not committed; cloned under ignored `.phase1-cache`):

- ZxwyWebSite/lx-script, revision `da7759eb54a9e293b5594933ebff61043e8c46cd`, MIT.
- File executed unchanged: `dist/lx-source-script.js`.
- Its companion service is not run. The PoC supplies deterministic mock HTTP responses through the sandbox bridge so compatibility can be tested without fetching music content.
# Bundled fonts

GXPlayer bundles Geist and Geist Mono through Fontsource packages. The font files are distributed under the SIL Open Font License 1.1. Simplified Chinese text uses the Windows system font fallback and does not add a bundled CJK font. The notices copied from the bundled packages are included in:

- `third_party/licenses/OFL-Geist.txt`
- `third_party/licenses/OFL-Geist-Mono.txt`
