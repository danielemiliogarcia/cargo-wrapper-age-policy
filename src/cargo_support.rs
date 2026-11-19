//! Detects whether the selected Cargo can enforce its native minimum-publish-age policy.

use crate::Result;
use std::path::Path;
use std::process::Command;

pub const STABLE_SINCE_MINOR: u64 = 100;
pub const UNSTABLE_FEATURE: &str = "min-publish-age";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoSupport {
    Stable,
    UnstableFlag,
    Unsupported,
}

impl CargoSupport {
    pub fn description(self) -> &'static str {
        match self {
            Self::Stable => "native stable support",
            Self::UnstableFlag => "native nightly support through -Z min-publish-age",
            Self::Unsupported => "old-Cargo compatibility filtering",
        }
    }
}

pub fn detect(real_cargo: &Path, toolchain: Option<&str>) -> Result<CargoSupport> {
    let version = run_probe(real_cargo, toolchain, &["-V"])?;
    let parsed = if version.status_success {
        parse_version(&version.stdout)
    } else {
        None
    };
    let Some(parsed) = parsed else {
        return Ok(CargoSupport::Unsupported);
    };
    if parsed.is_native() {
        return Ok(CargoSupport::Stable);
    }
    // Stable and beta Cargo list `min-publish-age` in `-Z help` from 1.97 on but
    // reject `-Z` outright, so only probe when the channel can accept the flag.
    if !parsed.nightly {
        return Ok(CargoSupport::Unsupported);
    }

    let unstable = run_probe(real_cargo, toolchain, &["-Z", "help"])?;
    if unstable.status_success
        && unstable
            .stdout
            .lines()
            .any(|line| line.contains(UNSTABLE_FEATURE))
    {
        return Ok(CargoSupport::UnstableFlag);
    }

    Ok(CargoSupport::Unsupported)
}

#[derive(Debug)]
struct ProbeOutput {
    status_success: bool,
    stdout: String,
}

fn run_probe(real_cargo: &Path, toolchain: Option<&str>, args: &[&str]) -> Result<ProbeOutput> {
    let mut command = Command::new(real_cargo);
    if let Some(toolchain) = toolchain {
        command.arg(toolchain);
    }
    let output = command.args(args).output().map_err(|error| {
        format!(
            "age-policy: cannot inspect Cargo at {}: {error}",
            real_cargo.display()
        )
    })?;
    Ok(ProbeOutput {
        status_success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CargoVersion {
    major: u64,
    minor: u64,
    /// True when the pre-release suffix starts with `nightly` or `dev`.
    nightly: bool,
}

impl CargoVersion {
    fn is_native(self) -> bool {
        self.major > 1 || (self.major == 1 && self.minor >= STABLE_SINCE_MINOR)
    }
}

/// Parses the version token of `cargo -V` output such as `cargo 1.99.0-nightly (abc 2026-08-01)`.
fn parse_version(output: &str) -> Option<CargoVersion> {
    let version = output.split_whitespace().nth(1)?;
    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    Some(CargoVersion {
        major,
        minor,
        nightly: prerelease
            .is_some_and(|value| value.starts_with("nightly") || value.starts_with("dev")),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn stable_old_release_is_neither_native_nor_nightly() {
        let version = parse_version("cargo 1.99.1 (5f94df478 2026-08-27)").unwrap();
        assert!(!version.is_native());
        assert!(!version.nightly);
    }

    #[test]
    fn old_nightly_is_detected_as_nightly() {
        let version = parse_version("cargo 1.99.0-nightly (abc 2026-08-01)").unwrap();
        assert!(!version.is_native());
        assert!(version.nightly);
        assert!(parse_version("cargo 1.99.0-dev (abc 2026-08-01)").unwrap().nightly);
    }

    #[test]
    fn beta_is_not_nightly() {
        assert!(!parse_version("cargo 1.99.0-beta.2 (abc date)").unwrap().nightly);
    }

    #[test]
    fn recognizes_native_versions() {
        let version = parse_version("cargo 1.100.0 (3acc79c0c 2026-11-05)").unwrap();
        assert!(version.is_native());
        assert!(!version.nightly);
        assert!(parse_version("cargo 1.101.2-nightly (abc date)").unwrap().is_native());
        assert!(parse_version("cargo 2.0.0 (abc date)").unwrap().is_native());
    }

    #[test]
    fn rejects_malformed_versions() {
        assert!(parse_version("cargo unknown").is_none());
        assert!(parse_version("not cargo output").is_none());
        assert!(parse_version("").is_none());
    }
}
