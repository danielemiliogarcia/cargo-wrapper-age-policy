//! Runs the age-policy middleware and delegates version selection to Cargo's native resolver.

use cargo_wrapper_age_policy::cargo_support::{CargoSupport, detect};
use cargo_wrapper_age_policy::config::{self, FallbackPolicy};
use cargo_wrapper_age_policy::fallback::{SparseFilterServer, locked_crates_io_packages};
use cargo_wrapper_age_policy::policy::{
    GLOBAL_AGE_ENV, apply_native_config, apply_support, explicitly_disabled,
    is_known_non_resolving, is_known_resolving, is_plugin_status_command, selected_toolchain,
};
use cargo_wrapper_age_policy::protocol::{self, ProtocolContext};
use std::env;
use std::process::exit;

fn direct_help() {
    println!(
        "cargo-wrapper-plugin-age-policy {}\n\n\
         Install this binary through Cargo Wrapper under the exact name `age-policy`.\n\
         Then configure Cargo's native `registry.global-min-publish-age` setting.\n\n\
         Commands:\n  cargo age-policy\n  cargo wrapper plugin install age-policy <local-binary>\n\
         \nThis executable rejects ordinary direct invocation because protocol v1 is required.",
        env!("CARGO_PKG_VERSION")
    );
}

fn print_status(support: Result<CargoSupport, String>, policy: Result<FallbackPolicy, String>) {
    println!("Cargo Wrapper Age Policy {}", env!("CARGO_PKG_VERSION"));
    println!("Registration name: age-policy");
    match support {
        Ok(support) => println!("Selected Cargo: {}", support.description()),
        Err(error) => println!("Selected Cargo: could not be inspected\n{error}"),
    }
    match policy {
        Ok(FallbackPolicy::Disabled) => println!("Effective minimum publish age: disabled"),
        Ok(FallbackPolicy::Enabled {
            cargo_key,
            raw,
            defaulted,
            ..
        }) => println!(
            "Effective minimum publish age: {raw:?} via {cargo_key}{}",
            if defaulted { " (plugin default)" } else { "" }
        ),
        Err(error) => println!("Effective minimum publish age: invalid\n{error}"),
    }
    println!(
        "Configure, for example:\n\n  [registry]\n  global-min-publish-age = \"30 days\"\n\n\
         Set the native value to \"0\" to disable filtering."
    );
}

fn run() -> Result<i32, String> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if !protocol::is_host_invocation() {
        if args.len() == 1 && matches!(args[0].as_str(), "-V" | "--version") {
            println!(
                "cargo-wrapper-plugin-age-policy {}",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(0);
        }
        if args.is_empty() || (args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help")) {
            direct_help();
            return Ok(0);
        }
        return Err(
            "age-policy: refusing direct middleware execution; install and invoke it through Cargo Wrapper protocol v1"
                .to_owned(),
        );
    }

    let context = ProtocolContext::from_environment()?;
    let toolchain = selected_toolchain(&args);

    if is_plugin_status_command(&args) {
        let cwd = env::current_dir().map_err(|error| error.to_string())?;
        print_status(
            detect(context.real_cargo(), toolchain),
            config::resolve(&args, &cwd),
        );
        return Ok(0);
    }

    if is_known_non_resolving(&args)
        || protocol::is_enabled_plugin_command(&args)
        || explicitly_disabled(&args, env::var(GLOBAL_AGE_ENV).ok().as_deref())
    {
        return context.run_next(&args);
    }

    let support = detect(context.real_cargo(), toolchain)?;
    let policy = config::resolve(
        &args,
        &env::current_dir().map_err(|error| error.to_string())?,
    )?;
    match policy {
        FallbackPolicy::Disabled => context.run_next(&args),
        FallbackPolicy::Enabled {
            cargo_key,
            raw,
            age,
            defaulted: _,
            fallback_blocker,
        } => match support {
            CargoSupport::Stable | CargoSupport::UnstableFlag => {
                args = apply_support(&args, support);
                args = apply_native_config(&args, cargo_key, &raw);
                context.run_next(&args)
            }
            CargoSupport::Unsupported => {
                if let Some(error) = fallback_blocker {
                    return Err(error);
                }
                if !is_known_resolving(&args) {
                    return Err(
                        "age-policy: old-Cargo compatibility mode refused an unknown Cargo subcommand because it cannot prove that nested dependency resolution will use the filtered index; use Cargo 1.100+ or disable the policy explicitly with a native value of `0`"
                            .to_owned(),
                    );
                }
                let locked = locked_crates_io_packages(
                    &args,
                    &env::current_dir().map_err(|error| error.to_string())?,
                )?;
                let server = SparseFilterServer::start(age, locked)?;
                args = apply_native_config(&args, cargo_key, &raw);
                let filtered_args = server.cargo_arguments(&args);
                eprintln!(
                    "age-policy: selected Cargo lacks native support; enforcing {raw:?} through a private crates.io sparse-index filter"
                );
                let status = context.run_next(&filtered_args);
                let filter_result = server.finish();
                filter_result?;
                status
            }
        },
    }
}

fn main() {
    match run() {
        Ok(code) => exit(code),
        Err(error) => {
            eprintln!("{error}");
            exit(1);
        }
    }
}
