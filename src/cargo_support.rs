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
    if version.status_success && stable_native_version(&version.stdout) {
        return Ok(CargoSupport::Stable);
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

fn stable_native_version(output: &str) -> bool {
    let Some(version) = output.split_whitespace().nth(1) else {
        return false;
    };
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.');
    let Some(major) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    major > 1 || (major == 1 && minor >= STABLE_SINCE_MINOR)
}

#[cfg(test)]
mod tests {
    use super::stable_native_version;

    #[test]
    fn recognizes_stabilized_versions() {
        assert!(stable_native_version("cargo 1.100.0 (abc 2026-10-01)"));
        assert!(stable_native_version("cargo 1.101.2-nightly (abc date)"));
        assert!(stable_native_version("cargo 2.0.0 (abc date)"));
    }

    #[test]
    fn rejects_old_or_malformed_versions() {
        assert!(!stable_native_version("cargo 1.99.0 (abc date)"));
        assert!(!stable_native_version("cargo unknown"));
        assert!(!stable_native_version("not cargo output"));
    }
}
