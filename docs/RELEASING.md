# Releasing Veil

The release workflow builds Windows x64, Linux x64 and a universal macOS package from a
version tag. It creates a draft after packages, checksums, release signatures and provenance
are complete. Publishing that draft makes it available to the updater. See
[CODE_SIGNING.md](CODE_SIGNING.md) for the separate operating-system signing arrangements.

## Prepare a version

1. Confirm the intended changes are merged on main and CI is green. Check existing tags and
   releases so the new version has not already been used.
2. Bump `[workspace.package].version` in `Cargo.toml`. Run `cargo update --workspace` to
   update the twelve local package versions in `Cargo.lock`; inspect the diff to ensure
   third-party dependencies did not change.
3. Date the Unreleased section of `CHANGELOG.md` as `X.Y.Z, dd/mm/yyyy`. Put compatibility
   changes where users will see them, especially wire-format changes requiring all devices
   to update. Refresh the current handoff if its version or next steps changed.
4. Run `python tools/licences.py --check` and `git diff --check`. Check locked Cargo metadata
   reports the intended version for every workspace member. Open the release PR and wait
   for Windows tests, Linux/macOS lint/build and supply-chain checks before merging it.

A version/documentation change does not need a new test mirroring the version string. CI
checks the actual release candidate; logic changes still need relevant regression coverage.

## Tag and build

Create `vX.Y.Z` at the merged release commit and push that tag. The workflow refuses a tag
that differs from the Cargo version. The `release` environment may require approval or
configured signing credentials; inspect the run rather than changing those protections.

The build uses a clean toolchain, with no release build cache. macOS builds both Apple
Silicon and Intel. A runner-capacity failure can be retried on the same tag. A source or
packaging fix needs a new commit and version: never move a published tag.

`workflow_dispatch` builds and signs workflow artifacts without creating a release. Use it
when a packaging change needs checking before a tag.

## Verify the draft

Expect these eight packages, plus `SHA256SUMS.txt` and its 64-byte Ed25519 signature:

| System | Files |
|---|---|
| Windows x64 | `veil-X.Y.Z-windows-x64.msi`, `veil-X.Y.Z-windows-x64-portable.zip` |
| macOS universal | `veil-X.Y.Z-macos-universal.dmg`, `veil-X.Y.Z-macos-universal.tar.gz` |
| Linux x64 | `veil-X.Y.Z-linux-x64.AppImage`, `.deb`, `.rpm`, `.tar.gz` |

Check the tag's commit, workflow conclusion, exact file names and nonzero sizes. The
checksums job refuses missing or extra packages, checks that the signing key matches
`crates/veil-app/release-key.pub`, signs the manifest, and verifies its signature. The
publish job checks every package hash and attaches GitHub build provenance.

Independently download the draft files. Verify the checksum signature using the trusted
public key, then verify every package's SHA-256 against that signed manifest. A checksum
downloaded beside a package alone does not authenticate its publisher. No private key
should be downloaded, printed or copied out of the release environment.

For a reproducible check, put all ten downloaded files in `release-check/` under an already
trusted checkout. From the repository root, use Python 3 and OpenSSL 3 to convert the raw
hex public key into a DER public key, then verify the exact manifest bytes:

```bash
python -c "from pathlib import Path; k=bytes.fromhex(Path('crates/veil-app/release-key.pub').read_text().strip()); Path('release-check/release-public.der').write_bytes(bytes.fromhex('302a300506032b6570032100')+k)"
openssl pkeyutl -verify -pubin -keyform DER -inkey release-check/release-public.der -rawin -in release-check/SHA256SUMS.txt -sigfile release-check/SHA256SUMS.txt.sig
```

Use the public key from the trusted checkout, not an unverified key downloaded beside the
packages. Continue only if OpenSSL exits successfully and reports signature verification
success. Then, in a Linux shell or Git Bash, check all package hashes:

```bash
cd release-check
sha256sum -c SHA256SUMS.txt
```

Every line must report `OK`. The DER file contains only the public key and can be removed
after checking. These commands do not install or run a release package.

Release-key signatures protect the updater. Authenticode and Apple Developer ID/notarisation
are separate: an updater signature does not remove SmartScreen or Gatekeeper warnings.
State the actual signing status; missing optional OS credentials produce unsigned/ad-hoc
packages. Never describe them as OS-signed just because the checksum signature is valid.

## Publish and check

Publishing needs the maintainer's authorization; a request to cut and publish a release
provides it. Replace generated notes with clear changes, upgrade requirements and known
limits. Publish the verified draft as a normal release and mark it latest.

Read back `draft=false`, `prerelease=false`, the tag and all ten assets. Confirm the
`releases/latest` endpoint returns this tag, because that is what the app queries. An older
installed copy should then offer the update; installation remains a separate user action.

Keep release verification results and any untested platform/device behaviour in the release
notes or handoff. The latest release and main are the supported security-fix lines.
