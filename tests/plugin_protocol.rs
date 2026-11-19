//! Exercises the executable protocol boundary with fake Cargo and continuation processes.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    cargo: PathBuf,
    next: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!(
            "cargo-wrapper-age-policy-test-{}-{nonce}-{id}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let compiled = compile_fixture(&root);
        let cargo = root.join(executable_name("fake-cargo"));
        let next = root.join(executable_name("next"));
        fs::copy(&compiled, &cargo).unwrap();
        fs::copy(&compiled, &next).unwrap();
        make_executable(&cargo);
        make_executable(&next);
        let trace = root.join("trace.txt");
        Self {
            root,
            cargo,
            next,
            trace,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-wrapper-plugin-age-policy"));
        command
            .env("CARGO_WRAPPER_PLUGIN_PROTOCOL", "1")
            .env("CARGO_WRAPPER_PLUGIN_NAME", "age-policy")
            .env("CARGO_WRAPPER_REAL_CARGO", &self.cargo)
            .env("CARGO_WRAPPER_NEXT", &self.next)
            .env("CARGO_WRAPPER_PLUGIN_CHAIN", "age-policy,dependency-audit")
            .env("FIXTURE_TRACE", &self.trace);
        command
    }

    fn trace(&self) -> String {
        fs::read_to_string(&self.trace).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn stable_cargo_runs_next_with_effective_native_config() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.100.0 (stable)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .args(["update", "--precise", "1.2.3"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        fixture.trace(),
        "next|--config registry.global-min-publish-age=\"30 days\" update --precise 1.2.3|age=30 days\n"
    );
}

#[test]
fn supporting_nightly_gets_native_unstable_flag() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0-nightly (nightly)")
        .env("FIXTURE_SUPPORTS_MIN_AGE", "1")
        .args(["+nightly-2026-06-21", "build"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        fixture.trace(),
        "next|+nightly-2026-06-21 --config registry.global-min-publish-age=\"30 days\" -Z min-publish-age build|age=<unset>\n"
    );
}

#[test]
fn stable_cargo_listing_the_unstable_flag_still_uses_the_fallback_filter() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.1 (5f94df478 2026-08-27)")
        .env("FIXTURE_SUPPORTS_MIN_AGE", "1")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .arg("build")
        .output()
        .unwrap();
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("private crates.io sparse-index filter"));
    let trace = fixture.trace();
    assert!(!trace.contains("-Z"), "unexpected -Z flag in {trace}");
    assert!(trace.contains("source.crates-io.replace-with=\"cargo-wrapper-age-policy\""));
}

#[test]
fn unsupported_cargo_uses_private_filter_before_next() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .arg("build")
        .output()
        .unwrap();
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("private crates.io sparse-index filter"));
    let trace = fixture.trace();
    assert!(trace.contains("source.crates-io.replace-with=\"cargo-wrapper-age-policy\""));
    assert!(trace.contains("source.cargo-wrapper-age-policy.registry=\"sparse+http://127.0.0.1:"));
    assert!(trace.ends_with(" build|age=30 days\n"));
}

#[test]
fn fallback_fails_closed_for_an_unfilterable_registry() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .args(["build", "--index", "https://example.invalid/index"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot guarantee minimum age for --index")
    );
    assert_eq!(fixture.trace(), "");
}

#[test]
fn fallback_fails_closed_for_unknown_subcommands() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .arg("third-party-command")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown Cargo subcommand"));
    assert_eq!(fixture.trace(), "");
}

#[test]
fn cargo_sort_is_the_default_bounded_external_passthrough() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "30 days")
        .args(["sort", "--workspace", "--check"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("passing through allowlisted external Cargo subcommand `sort`")
    );
    assert_eq!(
        fixture.trace(),
        "next|sort --workspace --check|age=30 days\n"
    );
}

#[test]
fn external_passthrough_can_be_replaced_or_disabled_explicitly() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env(
            "CARGO_WRAPPER_AGE_POLICY_PASSTHROUGH_COMMANDS",
            "third-party-command",
        )
        .arg("third-party-command")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(fixture.trace(), "next|third-party-command|age=<unset>\n");

    let disabled = Fixture::new();
    let output = disabled
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .env("CARGO_WRAPPER_AGE_POLICY_PASSTHROUGH_COMMANDS", "")
        .arg("sort")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown Cargo subcommand"));
    assert_eq!(disabled.trace(), "");
}

#[test]
fn downstream_plugin_administration_bypasses_dependency_filtering() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.99.0 (old-stable)")
        .arg("dependency-audit")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(fixture.trace(), "next|dependency-audit|age=<unset>\n");
}

#[test]
fn explicit_native_zero_disables_enforcement() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.1.0 (unsupported)")
        .env("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "0")
        .arg("build")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(fixture.trace(), "next|build|age=0\n");
}

#[test]
fn informational_commands_remain_available_on_old_cargo() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.1.0 (unsupported)")
        .arg("--version")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(fixture.trace(), "next|--version|age=<unset>\n");
}

#[test]
fn downstream_rejection_is_propagated() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.100.0 (stable)")
        .env("FIXTURE_NEXT_EXIT", "42")
        .arg("update")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        fixture.trace(),
        "next|--config registry.global-min-publish-age=\"30 days\" update|age=<unset>\n"
    );
}

#[test]
fn status_is_handled_without_running_next() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("FIXTURE_CARGO_VERSION", "cargo 1.100.0 (stable)")
        .arg("age-policy")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("native stable support"));
    assert_eq!(fixture.trace(), "");
}

#[test]
fn binary_rejects_an_incorrect_registration_name() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("CARGO_WRAPPER_PLUGIN_NAME", "wrong-name")
        .arg("build")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected \"age-policy\""));
    assert_eq!(fixture.trace(), "");
}

fn compile_fixture(directory: &Path) -> PathBuf {
    let source = directory.join("fixture.rs");
    let executable = directory.join(executable_name("fixture"));
    fs::write(
        &source,
        r#"
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::exit;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let stem = env::current_exe()
        .unwrap()
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_owned();

    if stem == "fake-cargo" {
        if args.last().map(String::as_str) == Some("-V") {
            println!("{}", env::var("FIXTURE_CARGO_VERSION").unwrap_or_else(|_| "cargo 1.0.0".to_owned()));
            return;
        }
        if args.ends_with(&["-Z".to_owned(), "help".to_owned()]) {
            if env::var_os("FIXTURE_SUPPORTS_MIN_AGE").is_some() {
                println!("    min-publish-age       Enable minimum publish age");
                return;
            }
            eprintln!("the `-Z` flag is only accepted on the nightly channel");
            exit(101);
        }
        exit(99);
    }

    let trace = env::var_os("FIXTURE_TRACE").unwrap();
    let age = env::var("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE")
        .unwrap_or_else(|_| "<unset>".to_owned());
    let mut file = OpenOptions::new().create(true).append(true).open(trace).unwrap();
    writeln!(file, "next|{}|age={age}", args.join(" ")).unwrap();
    let code = env::var("FIXTURE_NEXT_EXIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    exit(code);
}
"#,
    )
    .unwrap();

    let output = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert_success(&output);
    executable
}

fn executable_name(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
