//! Classifies invocations and adds Cargo's native unstable feature flag when required.

use crate::cargo_support::CargoSupport;

pub const GLOBAL_AGE_ENV: &str = "CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE";
pub const PASSTHROUGH_COMMANDS_ENV: &str = "CARGO_WRAPPER_AGE_POLICY_PASSTHROUGH_COMMANDS";
const DEFAULT_PASSTHROUGH_COMMANDS: &[&str] = &["sort"];
const MAX_PASSTHROUGH_COMMANDS: usize = 64;
const MAX_PASSTHROUGH_COMMAND_LENGTH: usize = 128;

pub fn selected_toolchain(args: &[String]) -> Option<&str> {
    args.first()
        .filter(|argument| argument.starts_with('+') && argument.len() > 1)
        .map(String::as_str)
}

pub fn is_plugin_status_command(args: &[String]) -> bool {
    cargo_subcommand(args).is_some_and(|command| command == "age-policy")
}

pub fn is_known_non_resolving(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "-V" | "--version" | "--list" | "--help" | "-h"
        )
    }) {
        return true;
    }
    matches!(
        cargo_subcommand(args),
        Some(
            "clean"
                | "help"
                | "init"
                | "locate-project"
                | "login"
                | "logout"
                | "new"
                | "owner"
                | "pkgid"
                | "read-manifest"
                | "report"
                | "search"
                | "uninstall"
                | "verify-project"
                | "version"
                | "yank"
        )
    )
}

pub fn is_known_resolving(args: &[String]) -> bool {
    matches!(
        cargo_subcommand(args),
        Some(
            "add"
                | "bench"
                | "build"
                | "check"
                | "clippy"
                | "doc"
                | "fetch"
                | "fix"
                | "forceadd"
                | "forcegenerate-lockfile"
                | "forceinstall"
                | "forceremove"
                | "forceupdate"
                | "generate-lockfile"
                | "install"
                | "metadata"
                | "package"
                | "publish"
                | "remove"
                | "run"
                | "rustc"
                | "rustdoc"
                | "test"
                | "tree"
                | "update"
                | "vendor"
        )
    )
}

pub fn allowlisted_external_command<'a>(
    args: &'a [String],
    configured: Option<&str>,
) -> crate::Result<Option<&'a str>> {
    if is_known_resolving(args) || is_known_non_resolving(args) {
        return Ok(None);
    }
    let Some(command) = cargo_subcommand(args) else {
        return Ok(None);
    };

    if let Some(configured) = configured {
        if configured.len() > MAX_PASSTHROUGH_COMMANDS * (MAX_PASSTHROUGH_COMMAND_LENGTH + 1) {
            return Err(format!(
                "age-policy: {PASSTHROUGH_COMMANDS_ENV} exceeds its bounded size"
            ));
        }
        let mut count = 0_usize;
        let mut allowed = false;
        for entry in configured.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            count += 1;
            if count > MAX_PASSTHROUGH_COMMANDS
                || entry.len() > MAX_PASSTHROUGH_COMMAND_LENGTH
                || !entry
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(format!(
                    "age-policy: invalid command in {PASSTHROUGH_COMMANDS_ENV}: {entry:?}"
                ));
            }
            allowed |= entry == command;
        }
        return Ok(allowed.then_some(command));
    }

    Ok(DEFAULT_PASSTHROUGH_COMMANDS
        .contains(&command)
        .then_some(command))
}

pub fn explicitly_disabled(args: &[String], environment_age: Option<&str>) -> bool {
    if environment_age.is_some_and(is_zero) {
        return true;
    }

    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if let Some(value) = argument.strip_prefix("--config=") {
            if config_disables_global_age(value) {
                return true;
            }
        } else if argument == "--config" {
            if args
                .get(index + 1)
                .is_some_and(|value| config_disables_global_age(value))
            {
                return true;
            }
            index += 1;
        }
        index += 1;
    }
    false
}

pub fn apply_support(args: &[String], support: CargoSupport) -> Vec<String> {
    if matches!(support, CargoSupport::Stable | CargoSupport::Unsupported)
        || has_unstable_age_flag(args)
    {
        return args.to_vec();
    }

    let insertion = usize::from(selected_toolchain(args).is_some());
    let mut result = Vec::with_capacity(args.len() + 2);
    result.extend_from_slice(&args[..insertion]);
    result.push("-Z".to_owned());
    result.push("min-publish-age".to_owned());
    result.extend_from_slice(&args[insertion..]);
    result
}

pub fn apply_native_config(args: &[String], cargo_key: &str, raw: &str) -> Vec<String> {
    let insertion = usize::from(selected_toolchain(args).is_some());
    let encoded = toml::Value::String(raw.to_owned()).to_string();
    let mut result = Vec::with_capacity(args.len() + 2);
    result.extend_from_slice(&args[..insertion]);
    result.push("--config".to_owned());
    result.push(format!("{cargo_key}={encoded}"));
    result.extend_from_slice(&args[insertion..]);
    result
}

pub fn cargo_subcommand(args: &[String]) -> Option<&str> {
    let mut index = usize::from(selected_toolchain(args).is_some());
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "--" {
            return None;
        }
        if argument == "-Z" || argument == "--config" {
            index += 2;
            continue;
        }
        if argument.starts_with("-Z")
            || argument.starts_with("--config=")
            || matches!(argument, "-v" | "-vv" | "--verbose" | "-q" | "--quiet")
        {
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(argument);
    }
    None
}

fn has_unstable_age_flag(args: &[String]) -> bool {
    args.iter().enumerate().any(|(index, argument)| {
        argument == "-Zmin-publish-age"
            || argument == "-Zmin-publish-age=yes"
            || (argument == "-Z"
                && args.get(index + 1).is_some_and(|value| {
                    value == "min-publish-age" || value == "min-publish-age=yes"
                }))
    })
}

fn config_disables_global_age(value: &str) -> bool {
    let Some((key, value)) = value.split_once('=') else {
        return false;
    };
    key.trim() == "registry.global-min-publish-age" && is_zero(value)
}

fn is_zero(value: &str) -> bool {
    value.trim().trim_matches(['\'', '"']) == "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn inserts_nightly_flag_after_toolchain() {
        assert_eq!(
            apply_support(&args(&["+nightly", "update"]), CargoSupport::UnstableFlag),
            args(&["+nightly", "-Z", "min-publish-age", "update"])
        );
    }

    #[test]
    fn does_not_duplicate_existing_flag() {
        let original = args(&["-Zmin-publish-age", "build"]);
        assert_eq!(
            apply_support(&original, CargoSupport::UnstableFlag),
            original
        );
    }

    #[test]
    fn recognizes_native_zero_overrides() {
        assert!(explicitly_disabled(&args(&["build"]), Some("0")));
        assert!(explicitly_disabled(
            &args(&["--config", "registry.global-min-publish-age='0'", "build"]),
            None
        ));
    }

    #[test]
    fn injects_effective_native_configuration() {
        assert_eq!(
            apply_native_config(
                &args(&["build"]),
                "registry.global-min-publish-age",
                "30 days"
            ),
            args(&[
                "--config",
                "registry.global-min-publish-age=\"30 days\"",
                "build"
            ])
        );
    }

    #[test]
    fn identifies_status_and_non_resolving_commands() {
        assert!(is_plugin_status_command(&args(&["age-policy"])));
        assert!(is_known_non_resolving(&args(&["--version"])));
        assert!(is_known_non_resolving(&args(&["clean"])));
        assert!(is_known_non_resolving(&args(&["init", "demo"])));
        assert!(!is_known_non_resolving(&args(&["build"])));
        assert!(is_known_resolving(&args(&["build"])));
        assert!(is_known_resolving(&args(&["forceadd", "serde"])));
        assert!(!is_known_resolving(&args(&["unknown-subcommand"])));
    }

    #[test]
    fn external_passthrough_is_narrow_configurable_and_cannot_bypass_resolving_commands() {
        assert_eq!(
            allowlisted_external_command(&args(&["sort", "--check"]), None).unwrap(),
            Some("sort")
        );
        assert_eq!(
            allowlisted_external_command(&args(&["third-party-command"]), None).unwrap(),
            None
        );
        assert_eq!(
            allowlisted_external_command(
                &args(&["third-party-command"]),
                Some("sort, third-party-command")
            )
            .unwrap(),
            Some("third-party-command")
        );
        assert_eq!(
            allowlisted_external_command(&args(&["sort"]), Some("")).unwrap(),
            None
        );
        assert_eq!(
            allowlisted_external_command(&args(&["build"]), Some("build")).unwrap(),
            None
        );
        assert!(allowlisted_external_command(&args(&["sort"]), Some("sort,../unsafe")).is_err());
    }
}
