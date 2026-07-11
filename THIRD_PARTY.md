# Third-party provenance

No third-party source code or data has been vendored into this repository yet.

Planned references:

- LX Music Desktop, Apache-2.0, used as a metadata implementation reference.
- Sollin Music Desktop, MIT, used as the factual LX source-runtime compatibility reference.

Any future copied or adapted code must record upstream repository, revision, original path, local path, license, and update procedure before merge.

Phase -1 external compatibility fixture (not committed; cloned under ignored `.phase1-cache`):

- ZxwyWebSite/lx-script, revision `da7759eb54a9e293b5594933ebff61043e8c46cd`, MIT.
- File executed unchanged: `dist/lx-source-script.js`.
- Its companion service is not run. The PoC supplies deterministic mock HTTP responses through the sandbox bridge so compatibility can be tested without fetching music content.
