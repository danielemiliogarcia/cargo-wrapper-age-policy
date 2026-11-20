<!-- Documents reproducible, attested binary releases for the standalone plugin. -->
# Releasing Cargo Wrapper Age Policy

Releases use checksum-pinned `cargo-dist` 0.32.0 and GitHub Actions. A `vMAJOR.MINOR.PATCH` tag builds native archives on Linux, macOS, and Windows, generates checksums and a source archive, attests every published file, and creates the GitHub Release. No installer is generated: users verify and extract an archive, then register the binary with Cargo Wrapper.

## Release targets

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

## Build the host archive locally

```sh
./scripts/install-pinned-dist.sh
dist --version
dist plan
dist build --artifacts=host --tag=v0.2.2
```

Use the version from `Cargo.toml`. Add `--allow-dirty` only for a private smoke test. Artifacts appear in `target/distrib/`.

```sh
cd target/distrib
sha256sum --check cargo-wrapper-age-policy-x86_64-unknown-linux-gnu.tar.xz.sha256
sha256sum --check source.tar.gz.sha256
tar -tJf cargo-wrapper-age-policy-x86_64-unknown-linux-gnu.tar.xz
```

A single host cannot build all native targets. The GitHub workflow supplies the Linux, macOS, and Windows matrix.

## Cut a release

```sh
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
dist plan
git tag -a v0.2.2 -m "Cargo Wrapper Age Policy 0.2.2"
git push origin v0.2.2
```

The workflow publishes only matching version tags. Any verification, build, packaging, or attestation failure prevents publication.

## Release trust model

The workflow downloads the exact cargo-dist 0.32.0 executable and checks its repository-pinned SHA-256 digest. External GitHub Actions are pinned to full commits. The publishing job alone receives attestation identity-token permission.

After downloading an artifact, verify both checksum and repository provenance:

```sh
gh attestation verify \
  cargo-wrapper-age-policy-x86_64-unknown-linux-gnu.tar.xz \
  --repo danielemiliogarcia/cargo-wrapper-age-policy \
  --signer-workflow danielemiliogarcia/cargo-wrapper-age-policy/.github/workflows/release.yml
```

Checksums detect corruption and attestations identify the producing repository and workflow. Neither proves that source code is safe.
