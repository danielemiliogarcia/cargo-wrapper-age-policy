//! Resolves the subset of native Cargo age configuration needed by the old-Cargo fallback.

use crate::Result;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Value;

pub const CRATES_IO_AGE_ENV: &str = "CARGO_REGISTRIES_CRATES_IO_MIN_PUBLISH_AGE";
pub const RESOLVER_POLICY_ENV: &str = "CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE";
pub const DEFAULT_MIN_PUBLISH_AGE: &str = "30 days";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FallbackPolicy {
    Disabled,
    Enabled {
        cargo_key: &'static str,
        raw: String,
        age: Duration,
        defaulted: bool,
        fallback_blocker: Option<String>,
    },
    /// A Cargo config file uses `include`, so file-derived values are incomplete and
    /// no CLI or environment value overrides them; native Cargo decides on its own.
    Deferred {
        include_path: PathBuf,
    },
}

#[derive(Default)]
struct MergedConfig {
    global_age: Option<String>,
    crates_io_age: Option<String>,
    resolver_policy: Option<String>,
    fallback_blocker: Option<String>,
    include_path: Option<PathBuf>,
}

/// Resolves the effective policy. When a Cargo config file uses `include`, fail closed if
/// `fail_on_include` is set (old-Cargo fallback); otherwise trust only CLI and environment
/// values, which outrank every config file, and defer the rest to native Cargo.
pub fn resolve(args: &[String], cwd: &Path, fail_on_include: bool) -> Result<FallbackPolicy> {
    let mut merged = MergedConfig::default();
    for path in cargo_config_paths(cwd) {
        merge_file(&path, &mut merged)?;
    }
    if let Some(include_path) = merged.include_path.take() {
        if fail_on_include {
            return Err(format!(
                "age-policy: old-Cargo compatibility mode cannot safely merge Cargo config `include` from {}; use {} or Cargo 1.100+",
                include_path.display(),
                crate::policy::GLOBAL_AGE_ENV
            ));
        }
        merged = MergedConfig {
            include_path: Some(include_path),
            ..MergedConfig::default()
        };
    }

    if let Ok(value) = env::var(crate::policy::GLOBAL_AGE_ENV) {
        merged.global_age = Some(value);
    }
    if let Ok(value) = env::var(CRATES_IO_AGE_ENV) {
        merged.crates_io_age = Some(value);
    }
    if let Ok(value) = env::var(RESOLVER_POLICY_ENV) {
        merged.resolver_policy = Some(value);
    }
    merge_cli(args, &mut merged)?;

    if merged.resolver_policy.as_deref() == Some("allow") {
        return Ok(FallbackPolicy::Disabled);
    }
    if let Some(include_path) = merged.include_path.clone()
        && merged.crates_io_age.is_none()
        && merged.global_age.is_none()
    {
        return Ok(FallbackPolicy::Deferred { include_path });
    }

    let (cargo_key, raw, defaulted) = if let Some(raw) = merged.crates_io_age {
        ("registries.crates-io.min-publish-age", raw, false)
    } else if let Some(raw) = merged.global_age {
        ("registry.global-min-publish-age", raw, false)
    } else {
        (
            "registry.global-min-publish-age",
            DEFAULT_MIN_PUBLISH_AGE.to_owned(),
            true,
        )
    };
    if raw == "0" {
        return Ok(FallbackPolicy::Disabled);
    }
    let age = parse_time_span(&raw).ok_or_else(|| {
        format!(
            "age-policy: invalid minimum publish age {raw:?}; expected `N seconds/minutes/hours/days/weeks/months` or `0`"
        )
    })?;
    Ok(FallbackPolicy::Enabled {
        cargo_key,
        raw,
        age,
        defaulted,
        fallback_blocker: merged.fallback_blocker,
    })
}

fn cargo_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(cargo_home) = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cargo")))
        && let Some(path) = preferred_config(&cargo_home)
    {
        result.push(path);
    }

    let mut ancestors: Vec<&Path> = cwd.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if let Some(path) = preferred_config(&ancestor.join(".cargo"))
            && !result.contains(&path)
        {
            result.push(path);
        }
    }
    result
}

fn preferred_config(directory: &Path) -> Option<PathBuf> {
    let toml = directory.join("config.toml");
    if toml.is_file() {
        return Some(toml);
    }
    let extensionless = directory.join("config");
    extensionless.is_file().then_some(extensionless)
}

fn merge_file(path: &Path, merged: &mut MergedConfig) -> Result<()> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("age-policy: cannot read {}: {error}", path.display()))?;
    let value: Value = toml::from_str(&contents)
        .map_err(|error| format!("age-policy: cannot parse {}: {error}", path.display()))?;

    if value.get("include").is_some() && merged.include_path.is_none() {
        merged.include_path = Some(path.to_owned());
    }
    if value
        .get("source")
        .and_then(|source| source.get("crates-io"))
        .and_then(|source| source.get("replace-with"))
        .is_some()
    {
        merged.fallback_blocker = Some(format!(
            "age-policy: old-Cargo compatibility mode will not override the existing crates.io source replacement in {}",
            path.display()
        ));
    }

    if let Some(raw) = string_at(&value, &["registry", "global-min-publish-age"]) {
        merged.global_age = Some(raw.to_owned());
    }
    if let Some(raw) = string_at(&value, &["registries", "crates-io", "min-publish-age"]) {
        merged.crates_io_age = Some(raw.to_owned());
    }
    if let Some(raw) = string_at(&value, &["resolver", "incompatible-publish-age"]) {
        merged.resolver_policy = Some(raw.to_owned());
    }
    Ok(())
}

fn merge_cli(args: &[String], merged: &mut MergedConfig) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let value = if let Some(value) = argument.strip_prefix("--config=") {
            Some(value)
        } else if argument == "--config" {
            index += 1;
            Some(
                args.get(index)
                    .ok_or_else(|| "age-policy: --config requires a value".to_owned())?
                    .as_str(),
            )
        } else {
            None
        };
        if let Some(value) = value {
            merge_cli_value(value, merged)?;
        }
        index += 1;
    }
    Ok(())
}

fn merge_cli_value(value: &str, merged: &mut MergedConfig) -> Result<()> {
    let Some((key, raw)) = value.split_once('=') else {
        return Ok(());
    };
    let parsed: Value = toml::from_str(&format!("value = {raw}")).map_err(|error| {
        format!("age-policy: cannot parse Cargo --config value {value:?}: {error}")
    })?;
    let Some(raw) = parsed.get("value").and_then(Value::as_str) else {
        return Ok(());
    };
    match key.trim() {
        "registry.global-min-publish-age" => merged.global_age = Some(raw.to_owned()),
        "registries.crates-io.min-publish-age" => merged.crates_io_age = Some(raw.to_owned()),
        "resolver.incompatible-publish-age" => merged.resolver_policy = Some(raw.to_owned()),
        "source.crates-io.replace-with" => {
            merged.fallback_blocker = Some(
                "age-policy: old-Cargo compatibility mode will not override a CLI crates.io source replacement"
                    .to_owned(),
            );
        }
        _ => {}
    }
    Ok(())
}

fn string_at<'a>(root: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = root;
    for part in path {
        current = current.get(*part)?;
    }
    current.as_str()
}

fn parse_time_span(span: &str) -> Option<Duration> {
    let right_index = span.find(|character: char| !character.is_ascii_digit())?;
    let (left, mut right) = span.split_at(right_index);
    if right.starts_with(' ') {
        right = &right[1..];
    }
    let count: u64 = left.parse().ok()?;
    let factor: u64 = match right {
        "second" | "seconds" => 1,
        "minute" | "minutes" => 60,
        "hour" | "hours" => 60 * 60,
        "day" | "days" => 24 * 60 * 60,
        "week" | "weeks" => 7 * 24 * 60 * 60,
        "month" | "months" => 2_629_746,
        _ => return None,
    };
    factor.checked_mul(count).map(Duration::from_secs)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let value = env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let value = env::var_os("HOME");
    value.map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::parse_time_span;
    use std::time::Duration;

    #[test]
    fn time_span_matches_cargo_units() {
        assert_eq!(parse_time_span("1second"), Some(Duration::from_secs(1)));
        assert_eq!(
            parse_time_span("2 weeks"),
            Some(Duration::from_secs(1_209_600))
        );
        assert_eq!(
            parse_time_span("6 months"),
            Some(Duration::from_secs(15_778_476))
        );
    }

    #[test]
    fn rejects_values_cargo_rejects() {
        assert_eq!(parse_time_span("30"), None);
        assert_eq!(parse_time_span(" 30 days"), None);
        assert_eq!(parse_time_span("30 days "), None);
        assert_eq!(parse_time_span("1.5 days"), None);
    }
}
