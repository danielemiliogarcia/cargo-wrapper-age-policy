<!-- Provides a host-safe Docker Sandbox procedure for the wrapper and both standalone plugins. -->
# Docker Sandbox smoke test

This procedure builds and installs Cargo Wrapper, Age Policy, and Dependency Audit entirely inside a disposable Docker Sandbox. It does not install Rust, Cargo Wrapper, or either plugin on the host.

The paths below assume these sibling repositories:

```text
/home/emi/tmp/cargowrapper
/home/emi/tmp/cargo-wrapper-age-policy
/home/emi/tmp/cargo-wrapper-dependency-audit
```

Adjust them if necessary.

## 1. Create the private clone

Run from the Age Policy repository on the host:

```sh
cd /home/emi/tmp/cargo-wrapper-age-policy

sbx create --clone \
  --name cargo-wrapper-age-policy-smoke \
  --memory 8g \
  shell .
```

`--clone` makes the primary workspace private inside the sandbox. An uncommitted repository may clone as an empty or older Git tree, so the next step explicitly copies the current unstaged source into `/tmp` inside the sandbox.

## 2. Copy the three current source trees

```sh
sbx exec cargo-wrapper-age-policy-smoke \
  mkdir -p /tmp/age-policy /tmp/cargowrapper /tmp/dependency-audit

for item in Cargo.toml Cargo.lock README.md LICENSE RELEASING.md dist-workspace.toml; do
  sbx cp "/home/emi/tmp/cargo-wrapper-age-policy/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/age-policy/"
done
for item in src tests scripts .github; do
  sbx cp "/home/emi/tmp/cargo-wrapper-age-policy/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/age-policy/"
done

for item in Cargo.toml Cargo.lock README.md LICENSE; do
  sbx cp "/home/emi/tmp/cargowrapper/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/cargowrapper/"
done
for item in src tests; do
  sbx cp "/home/emi/tmp/cargowrapper/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/cargowrapper/"
done

for item in Cargo.toml Cargo.lock README.md LICENSE factory-approvals.toml; do
  sbx cp "/home/emi/tmp/cargo-wrapper-dependency-audit/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/dependency-audit/"
done
for item in src tests; do
  sbx cp "/home/emi/tmp/cargo-wrapper-dependency-audit/$item" \
    "cargo-wrapper-age-policy-smoke:/tmp/dependency-audit/"
done
```

These are host-to-sandbox copies. Subsequent builds and installations affect only the sandbox.

## 3. Install the sandbox-only build prerequisites

```sh
sbx exec -u root cargo-wrapper-age-policy-smoke bash -lc '
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential git xz-utils ca-certificates
'

sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  curl --proto "=https" --tlsv1.2 --fail \
    https://sh.rustup.rs -o /tmp/rustup-init.sh
  sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain 1.96.0
  . "$HOME/.cargo/env"
  rustup toolchain install nightly-2026-07-29 --profile minimal
  rustup component add rustfmt clippy --toolchain 1.96.0
  cargo --version
  cargo +nightly-2026-07-29 --version
  cargo +nightly-2026-07-29 -Z help | grep min-publish-age
'
```

Stable 1.96 exercises compatibility filtering. The dated nightly exercises Cargo's official native implementation.

## 4. Build and install inside the sandbox

```sh
sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  set -euo pipefail
  . "$HOME/.cargo/env"

  cargo build --release --locked --manifest-path /tmp/cargowrapper/Cargo.toml
  cargo build --release --locked --manifest-path /tmp/age-policy/Cargo.toml
  cargo build --release --locked --manifest-path /tmp/dependency-audit/Cargo.toml

  mkdir -p "$HOME/.local/bin"
  install -m 0755 /tmp/cargowrapper/target/release/cargowrapper \
    "$HOME/.local/bin/cargo"
  export PATH="$HOME/.local/bin:$PATH"

  cargo wrapper
  cargo wrapper plugin install age-policy \
    /tmp/age-policy/target/release/cargo-wrapper-plugin-age-policy
  cargo wrapper plugin install dependency-audit \
    /tmp/dependency-audit/target/release/cargo-wrapper-plugin-dependency-audit
  cargo wrapper plugin list
'
```

Both plugins must initially appear as `inactive`.

## 5. Run the visible mock-registry selection test

```sh
sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  set -euo pipefail
  . "$HOME/.cargo/env"
  cd /tmp/age-policy
  ./scripts/demo-mock-age-selection.sh
'
```

The output must show that the registry's `0.0.2` release is only 29 days old, the policy withholds it, and Cargo downloads and compiles `0.0.1`, which is 31 days old. Both crate archives and the sparse registry are generated locally inside the sandbox.

## 6. Compare old Cargo with native Cargo

```sh
sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  set -euo pipefail
  . "$HOME/.cargo/env"
  export PATH="$HOME/.local/bin:$PATH"

  cargo wrapper plugin enable age-policy
  cargo age-policy

  cargo init --lib --name fallback_demo /tmp/fallback-demo
  cd /tmp/fallback-demo
  CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE="24 months" cargo add itoa
  fallback_version=$(awk '\''
    /name = "itoa"/{seen=1; next}
    seen && /version =/{gsub(/"/, "", $3); print $3; exit}
  '\'' Cargo.lock)

  cargo init --lib --name native_demo /tmp/native-demo
  cd /tmp/native-demo
  CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE="24 months" \
    cargo +nightly-2026-07-29 add itoa
  native_version=$(awk '\''
    /name = "itoa"/{seen=1; next}
    seen && /version =/{gsub(/"/, "", $3); print $3; exit}
  '\'' Cargo.lock)

  printf "fallback=%s native=%s\n" "$fallback_version" "$native_version"
  test "$fallback_version" = "$native_version"
'
```

At the time this guide was written, both paths selected `itoa 1.0.11` under the 24-month policy. The equality assertion is the durable check; the exact release will change as time passes.

## 7. Exercise the full plugin chain

```sh
sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  set -euo pipefail
  . "$HOME/.cargo/env"
  export PATH="$HOME/.local/bin:$PATH"

  cargo init --lib --name chain_demo /tmp/chain-demo
  cargo wrapper plugin enable dependency-audit
  cargo wrapper plugin list
  cargo age-policy
  cargo dependency-audit

  export CARGO_WRAPPER_DEPENDENCY_AUDIT_APPROVAL_FILE=/tmp/chain-approvals.toml

  cd /tmp/chain-demo
  cargo forceadd serde --features derive --dump-audit-prompt
  test -f audit-prompt-*.md
  grep -q "Review every artifact" audit-prompt-*.md
  test ! -s Cargo.lock

  cargo forceadd serde --features derive --accept-and-remember-all
  grep -q serde Cargo.toml
  grep -q '\''name = "serde"'\'' Cargo.lock
  cargo check
  cargo +nightly-2026-07-29 check
'
```

The list must show `age-policy` before `dependency-audit`, and both plugin status commands must remain reachable through the chain. The first dependency command must create an audit prompt without changing the project. The accepted command and both checks must succeed.

## 8. Verify the release archive

Inside SBX, create a temporary Git commit only for cargo-dist source provenance. This commit never reaches the host repository:

```sh
sbx exec cargo-wrapper-age-policy-smoke bash -lc '
  set -euo pipefail
  . "$HOME/.cargo/env"
  cd /tmp/age-policy

  git init -b main
  git config user.name "SBX Smoke"
  git config user.email "sbx@example.invalid"
  git add .
  git commit -m "temporary sandbox release input"

  ./scripts/install-pinned-dist.sh
  dist plan
  dist build --artifacts=host --tag=v0.1.0

  cd target/distrib
  sha256sum --check \
    cargo-wrapper-age-policy-x86_64-unknown-linux-gnu.tar.xz.sha256
  tar -tJf cargo-wrapper-age-policy-x86_64-unknown-linux-gnu.tar.xz
'
```

## 9. Remove the sandbox

```sh
sbx rm cargo-wrapper-age-policy-smoke
```

Removing it deletes the sandbox's Rust toolchains, Cargo caches, builds, installed wrapper, installed plugins, test projects, and temporary release commit.
