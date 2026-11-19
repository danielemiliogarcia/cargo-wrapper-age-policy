<!-- Explains native and compatibility enforcement as practical questions and answers. -->
# How does Cargo Wrapper Age Policy work?

> On Cargo 1.100 and newer this plugin is unnecessary because Cargo enforces minimum publish age natively.
> See the README section [Cargo 1.100 and newer](README.md#cargo-100-and-newer).

## What is the local HTTP server?

Think of it as a short-lived, read-only catalog filter between an old Cargo and the crates.io index. It is not a crates.io mirror, it is not a second dependency resolver, and it does not permanently run on your computer.

```text
Old Cargo
   │ requests available versions
   ▼
127.0.0.1:<random-port>
Age Policy filter
   │ downloads public index metadata over verified HTTPS
   ▼
https://index.crates.io
```

After Cargo selects an eligible version, it downloads the actual `.crate` archive directly from official crates.io storage:

```text
Cargo
   │ downloads the selected archive over HTTPS
   ▼
https://static.crates.io
```

The local server filters version metadata. It does not download, extract, compile, or execute crate source.

## When is the server needed?

Only when the selected Cargo is too old to support minimum publish age itself.

When Cargo has native stable support (1.100+), the plugin passes it the effective minimum-age configuration and Cargo enforces the policy directly. When a nightly Cargo exposes the feature, the plugin also supplies `-Z min-publish-age`. Neither native path starts the local server.

## Why are unused workspace dependencies ignored?

Cargo permits `[workspace.dependencies]` entries that no current workspace member inherits. Those declarations do not participate in the active dependency graph, and they may temporarily reference a missing local path during branch-specific development.

Compatibility mode now follows a workspace dependency definition only when a package uses `{ workspace = true }`. A missing path that is actually inherited still fails closed. This matches Cargo's treatment of unused workspace declarations without weakening checks for dependencies reachable from workspace members.

## How does the compatibility flow work?

Suppose crates.io contains:

```text
example 0.0.1: published 31 days ago
example 0.0.2: published 29 days ago
```

With a 30-day policy:

1. The plugin detects that the selected Cargo lacks native support.
2. It starts a temporary server on `127.0.0.1` using an operating-system-assigned port, such as `41837`.
3. It starts the next plugin or Cargo with temporary source-replacement settings.
4. Cargo asks the local server which versions of `example` exist.
5. The server fetches the original public index record from `https://index.crates.io` using verified TLS.
6. The filter withholds `0.0.2` because it is only 29 days old.
7. Cargo receives `0.0.1`, which is 31 days old and therefore eligible.
8. Cargo performs its normal SemVer resolution and selects `0.0.1`.
9. Cargo downloads the `0.0.1` archive directly from official crates.io storage and applies its normal checksum validation.
10. When the downstream command finishes, the plugin shuts down the local server.

Cargo asks for index records for every dependency it needs to resolve. The same filtering therefore applies to direct and transitive dependencies.

The server is only a filtered metadata window. It does not contain a second resolver, mirror all of crates.io, or choose a version for Cargo.

## How does crates.io end up resolving through my computer?

The plugin gives only that Cargo process temporary command-line configuration equivalent to:

```sh
cargo \
  --config 'source.crates-io.replace-with="cargo-wrapper-age-policy"' \
  --config 'source.cargo-wrapper-age-policy.registry="sparse+http://127.0.0.1:41837/"' \
  update
```

Cargo's source-replacement feature interprets this as: "For this command, obtain the crates.io version catalog from this temporary sparse-index address."

The server returns filtered crates.io metadata and tells Cargo to download selected archives from official crates.io storage. Cargo still records canonical crates.io package identities and checksums in `Cargo.lock`.

This is not DNS redirection. The name `crates.io` does not globally point to your computer.

## Does it change anything system-wide?

No. The plugin does not modify:

- DNS;
- `/etc/hosts`;
- browser or application traffic;
- router configuration;
- operating-system proxy settings; or
- permanent Cargo configuration.

The temporary source replacement applies only to the Cargo process started for that invocation. Other applications and unrelated Cargo processes continue reaching crates.io normally.

## Will the local server annoy me?

It does not remain running in the background:

- It is not a daemon or startup service.
- It starts only for a resolving operation on an old Cargo.
- It binds only to the loopback interface.
- The operating system chooses a currently free port.
- It requires no administrator privileges.
- It stops when the Cargo operation finishes.
- If the plugin process crashes, the operating system closes its socket.

During compatibility mode, you may notice a message saying the private filter is active and a small delay while index records pass through it.

Because each temporary server URL contains a new port, old-Cargo compatibility mode may also reduce Cargo registry-cache reuse. Some workflows may repeat metadata or archive downloads and leave additional Cargo cache entries. These costs disappear when Cargo provides native minimum-publish-age support because the plugin no longer starts the server.

## Can another computer connect to it?

No. The server binds to `127.0.0.1`, which is the local loopback interface, rather than a LAN or public network interface.

Other processes already running as your user can access loopback while the server exists, but the server exposes only public registry metadata. It does not need your project source or crates.io credentials.

## Why does Cargo use HTTP locally?

The local connection uses `http://127.0.0.1` because it never leaves your computer. The plugin's connection to the real crates.io index uses verified HTTPS.

The local listener already owns its randomly assigned port, so another ordinary process cannot bind to that same address and impersonate it. Cargo also retains its registry checksum validation for downloaded crate archives.

## What does “bounded” mean?

The server accepts only the small portion of HTTP required by Cargo's sparse-index client:

- only `GET` and `HEAD` requests;
- validated index paths;
- bounded request-header sizes;
- bounded upstream index responses; and
- no arbitrary filesystem paths or general-purpose proxy destinations.

Malformed data, unexpected requests, oversized responses, network failures, and TLS failures are errors. The plugin fails closed instead of silently continuing without the age policy.

## Does the local server choose the dependency version?

No. It only withholds versions that are too young. Cargo still performs its ordinary SemVer resolution and chooses the newest compatible version from the eligible records it receives.

The simplest mental model is:

> Age Policy filters the catalog. Cargo chooses from what remains.

## What happens to versions already in Cargo.lock?

The filter preserves versions already selected in `Cargo.lock`, matching Cargo's native minimum-publish-age behavior. Enabling or increasing the policy does not unexpectedly invalidate an existing reproducible lockfile.

The policy primarily controls newly resolved versions and updates.

## What happens when an index record has no publication timestamp?

The filter preserves it, matching Cargo's native behavior. Modern crates.io records normally include a publication timestamp, but historical records may not.

Allowing a record without a timestamp means its age cannot be proven from that metadata. Minimum publish age reduces exposure to newly published attacks; it does not prove that a dependency is safe.

## Will an exact young version be silently downgraded?

No. Suppose the manifest requires exactly:

```toml
example = "=0.0.2"
```

If `0.0.2` is too young, Cargo sees no eligible version satisfying the requirement and resolution fails. The plugin does not silently replace an exact request with `0.0.1`.

If the requirement admits both versions:

```toml
example = ">=0.0.1, <0.1.0"
```

Cargo sees the eligible `0.0.1`, withholds the young `0.0.2`, and selects `0.0.1` normally.

## What happens after Cargo stabilizes minimum publish age?

Cargo 1.100 has stabilized it. The compatibility server is not used there, and the plugin only passes the effective setting through as `--config`. Uninstall the plugin on Cargo 1.100+ and configure Cargo natively instead (see the README section [Cargo 1.100 and newer](README.md#cargo-100-and-newer)). Keep it only for projects and machines that still use Cargo 1.99 or older, until its end of support on 2027-06-30.
