# Release checklist

GXPlayer is Windows-only. Release artifacts must be built from a clean annotated
tag with Node 22 and the Rust toolchain pinned by `rust-toolchain.toml`.

```powershell
npm ci
npm run test:unit
npm run build
node scripts/check-version.mjs
cargo fmt --all -- --check
cargo test --workspace --locked
cargo test -p gx-streaming --no-default-features --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run tauri build
```

Before public distribution:

1. Confirm the installer contains `LICENSE`, `THIRD_PARTY.md`, and the files in
   `third_party/licenses`.
2. Sign the executable and installer with an Authenticode certificate using the
   organization's protected signing process. Do not store a PFX or password in
   the repository.
3. Verify signatures with `Get-AuthenticodeSignature` on every distributed
   executable, MSI, and NSIS installer.
4. Generate SHA-256 checksums with `Get-FileHash -Algorithm SHA256` and publish
   them next to the artifacts.
5. Smoke-test install, upgrade, uninstall, tray restore, Windows media controls,
   local playback, online fallback, cache relocation, and off-screen recovery on
   a clean Windows account.

Unsigned local development bundles must not be described as release-ready.
