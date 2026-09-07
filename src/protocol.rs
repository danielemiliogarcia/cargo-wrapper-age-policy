//! Validates Cargo Wrapper plugin protocol v1 and exposes its two executable paths.

use crate::Result;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const PROTOCOL_VERSION: &str = "1";
pub const PLUGIN_NAME: &str = "age-policy";

pub const ENV_PROTOCOL: &str = "CARGO_WRAPPER_PLUGIN_PROTOCOL";
pub const ENV_PLUGIN_NAME: &str = "CARGO_WRAPPER_PLUGIN_NAME";
pub const ENV_REAL_CARGO: &str = "CARGO_WRAPPER_REAL_CARGO";
pub const ENV_NEXT: &str = "CARGO_WRAPPER_NEXT";
pub const ENV_CHAIN: &str = "CARGO_WRAPPER_PLUGIN_CHAIN";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolContext {
    real_cargo: PathBuf,
    next: PathBuf,
}

impl ProtocolContext {
    pub fn from_environment() -> Result<Self> {
        let protocol = required_unicode(ENV_PROTOCOL)?;
        if protocol != PROTOCOL_VERSION {
            return Err(format!(
                "age-policy: unsupported plugin protocol {protocol:?}; expected {PROTOCOL_VERSION}"
            ));
        }

        let plugin_name = required_unicode(ENV_PLUGIN_NAME)?;
        if plugin_name != PLUGIN_NAME {
            return Err(format!(
                "age-policy: host registered this binary as {plugin_name:?}; expected {PLUGIN_NAME:?}"
            ));
        }

        Ok(Self {
            real_cargo: required_executable(ENV_REAL_CARGO)?,
            next: required_executable(ENV_NEXT)?,
        })
    }

    pub fn real_cargo(&self) -> &Path {
        &self.real_cargo
    }

    pub fn run_next(&self, args: &[String]) -> Result<i32> {
        let status = Command::new(&self.next)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                format!(
                    "age-policy: cannot invoke the next Cargo Wrapper stage at {}: {error}",
                    self.next.display()
                )
            })?;
        Ok(status.code().unwrap_or(1))
    }
}

pub fn is_host_invocation() -> bool {
    env::var_os(ENV_PROTOCOL).is_some()
}

pub fn is_enabled_plugin_command(args: &[String]) -> bool {
    let Some(command) = crate::policy::cargo_subcommand(args) else {
        return false;
    };
    env::var(ENV_CHAIN)
        .ok()
        .is_some_and(|chain| chain.split(',').any(|name| name == command))
}

fn required_unicode(name: &str) -> Result<String> {
    env::var_os(name)
        .ok_or_else(|| format!("age-policy: missing host variable {name}"))?
        .into_string()
        .map_err(|_| format!("age-policy: host variable {name} is not Unicode"))
}

fn required_executable(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(
        env::var_os(name).ok_or_else(|| format!("age-policy: missing host variable {name}"))?,
    );
    if !path.is_absolute() {
        return Err(format!(
            "age-policy: host variable {name} must be an absolute path"
        ));
    }
    let metadata = std::fs::metadata(&path).map_err(|error| {
        format!(
            "age-policy: executable from {name} is unavailable at {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "age-policy: executable from {name} is not a regular file: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "age-policy: executable from {name} lacks execute permission: {}",
                path.display()
            ));
        }
    }
    Ok(path)
}
