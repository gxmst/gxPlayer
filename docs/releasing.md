# Release checklist

GXPlayer is Windows-only. Release artifacts must be built from a clean annotated
tag with Node 22 and the Rust toolchain pinned by `rust-toolchain.toml`.

Verify the release identity before installing dependencies or producing artifacts.
The tag name must be `v` followed by the manifest version, it must be annotated,
and it must point exactly at `HEAD`:

```powershell
$version = (Get-Content package.json -Raw | ConvertFrom-Json).version
$tag = "v$version"
if (git status --porcelain) { throw "Release worktree is not clean." }
git rev-parse --verify "refs/tags/$tag^{tag}" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "$tag is missing or is not an annotated tag." }
if ((git rev-list -n 1 $tag) -ne (git rev-parse HEAD)) {
    throw "$tag does not point at HEAD."
}
```

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

For public signed artifacts, provide the protected Authenticode signer to Tauri
through `bundle.windows.signCommand` in a private CI/build configuration before
the final `npm run tauri build`. Tauri must sign each executable at the point it
is embedded and then sign the finished MSI and NSIS bundles. Signing only
`target/release/gxplayer.exe` after bundling does not sign the executable already
inside the installers. Never store a PFX or password in the repository.

Before public distribution:

1. Regenerate and review complete notices for all shipped Rust, npm, font, and
   data dependencies. Confirm the installer contains those notices together
   with `LICENSE`, `THIRD_PARTY.md`, and the files in `third_party/licenses`.
2. Verify signatures with `Get-AuthenticodeSignature` on every distributed
   executable, MSI, and NSIS installer, and on the installed `gxplayer.exe`
   from each installer type.
3. Generate SHA-256 checksums with `Get-FileHash -Algorithm SHA256` and publish
   them next to the artifacts.
4. Smoke-test install, upgrade, uninstall, tray restore, Windows media controls,
   local playback, online fallback, cache relocation, and off-screen recovery on
   a clean Windows account.

The NSIS setup executable is the primary end-user installer. The MSI is retained
for managed deployment. Same-format upgrades must be tested for both formats;
MSI-to-NSIS migration must also be tested. WiX does not automatically remove an
existing NSIS registration, so users switching from NSIS to MSI must uninstall
the NSIS build first. Do not present the two formats as interchangeable upgrade
paths.

Unsigned local development bundles must not be described as release-ready.
