<!-- Explains the plugin's native-Cargo policy, installation, composition, and security limits. -->
# Cargo Wrapper Age Policy

`cargo-wrapper-age-policy` is a protocol v1 middleware plugin for Cargo Wrapper. It makes Cargo's native minimum-publish-age resolver policy a required capability for dependency-resolving commands.

Once enabled, the plugin defaults to a 30-day minimum age. Installing it leaves it inactive, following Cargo Wrapper's normal plugin lifecycle; enabling it activates the default immediately.

The plugin does not implement a second dependency resolver and does not label versions safe. A release being older reduces exposure to just-published supply-chain attacks, but age is not proof that its code is trustworthy.

## Why devs should use this plugin

> Cargo provides the mechanism. Age Policy makes it the default.

A compromised crate release is most dangerous immediately after publication, before maintainers and the wider community have had time to notice and investigate it. Age Policy adds a configurable observation period before a newly published version can enter a newly resolved dependency graph.

With the default 30-day policy, Cargo still selects the newest compatible version, but only from releases old enough to satisfy the policy. This reduces exposure to just-published supply-chain attacks without replacing Cargo's resolver or requiring developers to select every dependency version manually.

Use it now because it:

- brings minimum-publish-age protection to older stable Cargo versions;
- covers direct and transitive dependencies;
- preserves normal Cargo SemVer resolution;
- provides a secure 30-day default that can be changed through native Cargo configuration;
- fails closed when compatibility filtering cannot guarantee policy coverage;
- composes with Dependency Audit, which reviews the exact artifacts selected after age filtering; and
- can be installed as a precompiled binary, avoiding the bootstrap problem of downloading the plugin's dependencies before its protection is available.

### Why keep it after Cargo stabilizes minimum publish age?

Native Cargo provides a capability, but the plugin ensures that the capability is consistently enabled, configured, and visible. Once the selected Cargo has stable support, Age Policy stops using compatibility filtering and delegates enforcement directly to Cargo's native resolver.

It remains useful as a thin policy layer because it:

- supplies the 30-day default when no Cargo setting is present;
- prevents new workstations, repositories, containers, and CI runners from silently forgetting the policy;
- provides consistent behavior while an organization operates a mixture of old and new Cargo versions;
- uses Cargo's native configuration instead of creating a separate resolver or configuration ecosystem;
- exposes the effective policy and enforcement path through `cargo age-policy`;
- keeps fail-closed behavior when enforcement cannot be guaranteed; and
- remains the version-age layer before other plugins, such as Dependency Audit, inspect the resulting exact graph.

The long-term role of the plugin is straightforward: require and communicate the organization's policy while native Cargo performs the resolution.

If every developer machine, CI runner, container, and repository eventually uses a sufficiently new Cargo and centrally managed Cargo configuration already guarantees the same policy, the plugin may become optional. Until then, it closes the gap between Cargo supporting a security control and an organization reliably applying that control everywhere.

## What it enforces

When the selected Cargo supports the policy, the plugin delegates selection to Cargo itself:

- Cargo ignores versions newer than the configured age and chooses the newest compatible eligible version.
- The rule covers the complete graph Cargo resolves, including transitive dependencies.
- Existing lockfile selections, registry entries without a publication timestamp, per-registry overrides, and Cargo's explicit `resolver.incompatible-publish-age = "allow"` override retain their native Cargo behavior.
- A value of `"0"` disables the age filter, just as it does in Cargo.

When the selected Cargo predates stable support, the plugin safely probes `cargo -Z help`. If that Cargo exposes `min-publish-age`, the plugin adds Cargo's native `-Z min-publish-age` flag before continuing.

For an older stable Cargo without that flag, the plugin starts a bounded private HTTP server on `127.0.0.1` and temporarily replaces only the crates.io sparse index for that invocation. The filter downloads index records over verified TLS, preserves locked versions and records without publication timestamps, and withholds releases newer than the configured age. The old Cargo still performs its own normal SemVer resolution, but it sees only eligible releases and therefore automatically chooses the newest compatible old-enough direct and transitive versions. See [How Cargo Wrapper Age Policy works](HOW_IT_WORKS.md) for the complete request flow, local-server lifecycle, security boundaries, and compatibility-mode tradeoffs.

If compatibility filtering cannot guarantee policy coverage, the operation fails closed: the next plugin and Cargo are not invoked. This includes alternate registries, an existing crates.io source replacement, Cargo configuration includes the plugin cannot safely merge, malformed index data, oversized responses, network/TLS failures, and unsafe workspace shapes the fallback cannot inspect. Switching to Cargo 1.100+ removes the registry-filter limitations because Cargo then owns the complete resolver policy.

Cargo informational commands such as `cargo --version`, `cargo help`, `cargo clean`, and `cargo locate-project` remain usable on an unsupported Cargo. The external non-resolving command `cargo sort` is also passed through by default so manifest-formatting hooks work on old Cargo.

Upstream Cargo currently marks the feature as stabilized for Cargo 1.100. Before that toolchain is available, use a nightly whose `cargo -Z help` output lists `min-publish-age`.

## Configure Cargo

The effective setting uses this precedence: command-line Cargo `--config`, Cargo's native environment, project or user Cargo configuration, then the plugin's `30 days` default. There is no separate plugin-only duration format.

For a project override, create `.cargo/config.toml`:

```toml
[registry]
global-min-publish-age = "14 days"
```

Named registries can override the global value:

```toml
[registries.my-registry]
min-publish-age = "7 days"
```

Cargo accepts integer values with `seconds`, `minutes`, `hours`, `days`, `weeks`, or `months`. Set a scope to `"0"` to disable filtering for it.

For a system-wide policy that must also survive another plugin's isolated `CARGO_HOME`, set Cargo's native environment key in the shell profile or organization-managed environment:

```sh
export CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE="60 days"
```

This is particularly useful with Dependency Audit, which intentionally resolves its candidate in a private temporary Cargo home. Age Policy resolves the effective global or crates.io value and passes it downstream as native Cargo configuration, so a value from the user's Cargo-home config also reaches that isolated candidate. Named alternate-registry policies require native Cargo 1.100+ and should be supplied in workspace configuration, inline `--config`, or environment when another plugin isolates `CARGO_HOME`.

For a one-command setting:

```sh
cargo --config 'registry.global-min-publish-age="7 days"' forceupdate
```

### External Cargo subcommands

Old-Cargo compatibility mode rejects unknown Cargo subcommands because the plugin cannot prove that an arbitrary external tool will preserve minimum-age filtering for nested Cargo calls. `cargo sort` is the single default exception because it formats manifests without resolving dependencies.

Replace the exact comma-separated exception list with:

```sh
export CARGO_WRAPPER_AGE_POLICY_PASSTHROUGH_COMMANDS="sort,another-reviewed-tool"
```

Set it to an empty value to reject every unknown external subcommand:

```sh
export CARGO_WRAPPER_AGE_POLICY_PASSTHROUGH_COMMANDS=""
```

The setting applies only to otherwise unknown external subcommands. It cannot bypass the age policy for resolving commands such as `build`, `test`, `update`, or `install`. An allowlisted external executable runs without the age filter; review and trust that tool because any Cargo process it starts itself is outside this invocation's policy chain.

## Unsupported Cargo and older alternatives

If an old Cargo cannot use native enforcement, the compatibility filter lets that same Cargo continue and automatically select the newest eligible versions. If the fallback cannot cover the requested source or configuration, it stops before dependency resolution and tells the developer to use Cargo 1.100+ or a supporting nightly.

If Cargo reports that all matching releases are too new, lower the direct dependency requirement to an older compatible release and resolve again under the capable Cargo. The plugin deliberately does not offer `cargo add name@older` as an unsupported-Cargo bypass: an old direct version may still select a newly published transitive dependency.

## Install a precompiled release

Precompiled binaries avoid downloading and compiling this plugin's own HTTP, TLS, JSON, and TOML dependencies before the policy is active. Verified release artifacts also give users one reproducible installation path.

1. Download the archive and its `.sha256` file for your platform from the GitHub Release.
2. Verify the checksum and repository provenance.
3. Extract `cargo-wrapper-plugin-age-policy` or `cargo-wrapper-plugin-age-policy.exe`.
4. Register it under the exact name `age-policy`, then enable it. Enabling activates the 30-day default unless Cargo configuration overrides it:

```sh
cargo wrapper plugin install age-policy /path/to/cargo-wrapper-plugin-age-policy
cargo wrapper plugin enable age-policy
cargo wrapper plugin list
cargo age-policy
```

On Windows, use the `.exe` path. Installation leaves the plugin inactive until the explicit `enable` command.

To disable or remove it:

```sh
cargo wrapper plugin disable age-policy
cargo wrapper plugin uninstall age-policy
```

## Build from source

Read and audit the source first. Then build in an isolated sandbox if the Cargo installation being used does not yet enforce minimum age:

```sh
cargo build --release --locked
cargo wrapper plugin install age-policy \
  ./target/release/cargo-wrapper-plugin-age-policy
cargo wrapper plugin enable age-policy
```

Building from source downloads third-party crates before this plugin can protect Cargo. Use an isolated sandbox for that bootstrap build unless you have already audited and vendored those exact dependencies.

## Visible mock-registry test

The repository includes a deterministic end-to-end demonstration with two harmless locally packaged versions. Its synthetic clock makes `0.0.2` exactly 29 days old and `0.0.1` exactly 31 days old. The test applies the 30-day index filter, asks real Cargo for the newest compatible version, and proves that Cargo downloads and compiles only `0.0.1`:

```sh
./scripts/demo-mock-age-selection.sh
```

An exact requirement such as `=0.0.2` is never silently downgraded; it fails resolution when that exact release is too young. The demonstration uses a compatible range that admits both releases.


## Composition with Dependency Audit

Install this binary under `age-policy` and the other plugin under `dependency-audit`. Cargo Wrapper protocol v1 runs enabled plugins in ascending registration-name order, so the chain is deterministic:

```text
age-policy -> dependency-audit -> Cargo
```

Age Policy first activates Cargo's native version filter or its old-Cargo compatibility index. Dependency Audit then resolves and reviews the resulting age-eligible candidate graph. Both plugins preserve downstream rejection and exit status. Each plugin's own status or administration command also remains reachable through earlier middleware.

`forceadd`, `forceupdate`, `forcegenerate-lockfile`, normal locked builds, and the other Dependency Audit guarded operations therefore retain their audit behavior. On a nightly toolchain, Dependency Audit also understands the leading `-Z min-publish-age` Cargo option added by this plugin.

Cargo intentionally does not apply minimum publish age to remote `cargo install`. Age Policy does not claim otherwise. When both plugins are enabled, Dependency Audit still blocks direct installation unless the developer uses its prominent explicit unreviewed-install-risk acceptance path.

## Direct invocation

The executable acts as middleware only when `CARGO_WRAPPER_PLUGIN_PROTOCOL` is exactly `1` and the host registers it as `age-policy`. Direct `--help` and `--version` are available, but direct policy execution is rejected to prevent accidental protocol bypass.

## Upstream design references

The implementation was checked against Cargo source revision `3c0b534756e166d12eb9fd2e1abfe5b42ac6101e` from 2026-09-04:

- [Cargo minimum-publish-age tracking issue](https://github.com/rust-lang/cargo/issues/17009)
- [Cargo resolver policy implementation](https://github.com/rust-lang/cargo/blob/3c0b534756e166d12eb9fd2e1abfe5b42ac6101e/src/resolver/version_prefs.rs)
- [Cargo's minimum-publish-age test suite](https://github.com/rust-lang/cargo/blob/3c0b534756e166d12eb9fd2e1abfe5b42ac6101e/tests/testsuite/min_publish_age.rs)
- [Cargo unstable feature documentation](https://doc.rust-lang.org/cargo/reference/unstable.html#min-publish-age)
- [Cargo Wrapper protocol v1](https://github.com/danielemiliogarcia/cargowrapper/blob/plugin-support/PLUGIN-PROTOCOL.md)
