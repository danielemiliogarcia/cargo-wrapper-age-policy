//! Filters the crates.io sparse index for Cargo versions without native age-policy support.

use crate::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UPSTREAM_INDEX: &str = "https://index.crates.io";
const MAX_REQUEST_HEADERS: usize = 16 * 1024;
const MAX_INDEX_RESPONSE: u64 = 32 * 1024 * 1024;
const SOURCE_NAME: &str = "cargo-wrapper-age-policy";

#[derive(Clone)]
struct Filter {
    cutoff: i128,
    locked: Arc<HashSet<(String, String)>>,
}

pub struct SparseFilterServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl SparseFilterServer {
    pub fn start(age: Duration, locked: HashSet<(String, String)>) -> Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                format!("age-policy: system clock predates the Unix epoch: {error}")
            })?;
        let filter = Filter {
            cutoff: i128::from(now.as_secs()) - i128::from(age.as_secs()),
            locked: Arc::new(locked),
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
            format!("age-policy: cannot bind private sparse-index filter: {error}")
        })?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("age-policy: cannot inspect sparse-index filter: {error}"))?;
        listener.set_nonblocking(true).map_err(|error| {
            format!("age-policy: cannot configure sparse-index filter: {error}")
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_errors = Arc::clone(&errors);
        let thread = thread::Builder::new()
            .name("cargo-age-policy-index".to_owned())
            .spawn(move || serve(listener, filter, worker_stop, worker_errors))
            .map_err(|error| format!("age-policy: cannot start sparse-index filter: {error}"))?;

        Ok(Self {
            address,
            stop,
            errors,
            thread: Some(thread),
        })
    }

    pub fn cargo_arguments(&self, args: &[String]) -> Vec<String> {
        let insertion = usize::from(crate::policy::selected_toolchain(args).is_some());
        let mut result = Vec::with_capacity(args.len() + 4);
        result.extend_from_slice(&args[..insertion]);
        result.push("--config".to_owned());
        result.push(format!("source.crates-io.replace-with=\"{SOURCE_NAME}\""));
        result.push("--config".to_owned());
        result.push(format!(
            "source.{SOURCE_NAME}.registry=\"sparse+http://{}/\"",
            self.address
        ));
        result.extend_from_slice(&args[insertion..]);
        result
    }

    pub fn finish(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address).and_then(|stream| stream.shutdown(Shutdown::Both));
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "age-policy: sparse-index filter thread panicked".to_owned())?;
        }
        let errors = self
            .errors
            .lock()
            .map_err(|_| "age-policy: sparse-index error state was poisoned".to_owned())?;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "age-policy: sparse-index filtering failed closed:\n  - {}",
                errors.join("\n  - ")
            ))
        }
    }
}

impl Drop for SparseFilterServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address).and_then(|stream| stream.shutdown(Shutdown::Both));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    filter: Filter,
    stop: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
) {
    let mut requests = Vec::new();
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let filter = filter.clone();
                let errors = Arc::clone(&errors);
                requests.push(thread::spawn(move || {
                    if let Err(error) = handle_request(stream, &filter)
                        && let Ok(mut values) = errors.lock()
                    {
                        values.push(error);
                    }
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                if let Ok(mut values) = errors.lock() {
                    values.push(format!("cannot accept Cargo index request: {error}"));
                }
                break;
            }
        }
    }
    for request in requests {
        if request.join().is_err()
            && let Ok(mut values) = errors.lock()
        {
            values.push("index request handler panicked".to_owned());
        }
    }
}

fn handle_request(mut stream: TcpStream, filter: &Filter) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| format!("cannot set index request timeout: {error}"))?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("cannot read Cargo index request: {error}"))?;
        if count == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > MAX_REQUEST_HEADERS {
            write_response(&mut stream, 431, "text/plain", b"request headers too large")?;
            return Err("Cargo index request headers exceeded the safety limit".to_owned());
        }
    }

    let request_text = std::str::from_utf8(&request)
        .map_err(|_| "Cargo index request headers were not UTF-8".to_owned())?;
    let request_line = request_text
        .lines()
        .next()
        .ok_or_else(|| "Cargo index request was empty".to_owned())?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if !matches!(method, "GET" | "HEAD") {
        write_response(&mut stream, 405, "text/plain", b"method not allowed")?;
        return Err(format!("unexpected Cargo index HTTP method {method:?}"));
    }
    let path = target.split('?').next().unwrap_or(target);
    if !valid_index_path(path) {
        write_response(&mut stream, 400, "text/plain", b"invalid index path")?;
        return Err(format!("refused unexpected Cargo index path {path:?}"));
    }

    let body = if path == "/config.json" {
        br#"{"dl":"https://static.crates.io/crates","api":"https://crates.io"}"#.to_vec()
    } else {
        let upstream = download_index_path(path)?;
        filter_index(&upstream, filter)?
    };
    if method == "HEAD" {
        write_response(&mut stream, 200, "text/plain", b"")
    } else {
        write_response(&mut stream, 200, "text/plain", &body)
    }
}

fn valid_index_path(path: &str) -> bool {
    if path == "/config.json" {
        return true;
    }
    path.starts_with('/')
        && path.len() <= 512
        && !path.contains("..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
}

fn download_index_path(path: &str) -> Result<Vec<u8>> {
    let response = ureq::get(&format!("{UPSTREAM_INDEX}{path}"))
        .call()
        .map_err(|error| format!("cannot download crates.io sparse index path {path}: {error}"))?;
    let mut reader = response.into_reader().take(MAX_INDEX_RESPONSE + 1);
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|error| format!("cannot read crates.io sparse index path {path}: {error}"))?;
    if body.len() as u64 > MAX_INDEX_RESPONSE {
        return Err(format!(
            "crates.io sparse index path {path} exceeded the {} MiB safety limit",
            MAX_INDEX_RESPONSE / 1024 / 1024
        ));
    }
    Ok(body)
}

#[derive(Deserialize)]
struct IndexEntry<'a> {
    name: &'a str,
    vers: &'a str,
    #[serde(default)]
    pubtime: Option<&'a str>,
}

fn filter_index(body: &[u8], filter: &Filter) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(body)
        .map_err(|_| "crates.io returned non-UTF-8 sparse index data".to_owned())?;
    let mut output = Vec::with_capacity(body.len());
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let entry: IndexEntry<'_> = serde_json::from_str(line)
            .map_err(|error| format!("cannot parse crates.io sparse index entry: {error}"))?;
        let locked = filter
            .locked
            .contains(&(entry.name.to_owned(), entry.vers.to_owned()));
        let eligible = match entry.pubtime {
            None => true,
            Some(value) => parse_rfc3339(value)? <= filter.cutoff,
        };
        if locked || eligible {
            output.extend_from_slice(line.as_bytes());
            output.push(b'\n');
        }
    }
    Ok(output)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .and_then(|()| stream.write_all(body))
    .and_then(|()| stream.flush())
    .map_err(|error| format!("cannot answer Cargo index request: {error}"))
}

pub fn locked_crates_io_packages(args: &[String], cwd: &Path) -> Result<HashSet<(String, String)>> {
    reject_explicit_alternate_registry(args)?;
    validate_manifest_registries(args, cwd)?;
    let Some(lock_path) = find_lockfile(args, cwd) else {
        return Ok(HashSet::new());
    };
    let contents = fs::read_to_string(&lock_path)
        .map_err(|error| format!("age-policy: cannot read {}: {error}", lock_path.display()))?;
    let lock: Lockfile = toml::from_str(&contents)
        .map_err(|error| format!("age-policy: cannot parse {}: {error}", lock_path.display()))?;
    let mut locked = HashSet::new();
    for package in lock.package {
        let Some(source) = package.source else {
            continue;
        };
        if is_crates_io_source(&source) {
            locked.insert((package.name, package.version));
        } else if source.starts_with("registry+") {
            return Err(format!(
                "age-policy: old-Cargo compatibility mode cannot guarantee minimum age for alternate registry source {source:?}; use Cargo 1.100+"
            ));
        }
    }
    Ok(locked)
}

#[derive(Deserialize)]
struct Lockfile {
    #[serde(default)]
    package: Vec<LockedPackage>,
}

#[derive(Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
}

fn is_crates_io_source(source: &str) -> bool {
    matches!(
        source,
        "registry+https://github.com/rust-lang/crates.io-index"
            | "registry+https://index.crates.io/"
    )
}

fn reject_explicit_alternate_registry(args: &[String]) -> Result<()> {
    for (index, argument) in args.iter().enumerate() {
        if argument == "--index" || argument.starts_with("--index=") {
            return Err(
                "age-policy: old-Cargo compatibility mode cannot guarantee minimum age for --index; use Cargo 1.100+"
                    .to_owned(),
            );
        }
        if (argument == "--registry"
            && args
                .get(index + 1)
                .is_some_and(|registry| registry != "crates-io"))
            || argument
                .strip_prefix("--registry=")
                .is_some_and(|registry| registry != "crates-io")
        {
            return Err(
                "age-policy: old-Cargo compatibility mode currently supports crates.io only; use Cargo 1.100+ for alternate registries"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn validate_manifest_registries(args: &[String], cwd: &Path) -> Result<()> {
    let Some(root_manifest) = find_manifest(args, cwd) else {
        return Ok(());
    };
    let mut pending = vec![root_manifest];
    let mut visited = HashSet::new();
    let mut workspace_dependencies = None;
    while let Some(path) = pending.pop() {
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "age-policy: cannot resolve manifest {} while checking registry coverage: {error}",
                path.display()
            )
        })?;
        if !visited.insert(canonical.clone()) {
            continue;
        }
        if visited.len() > 4_096 {
            return Err(
                "age-policy: old-Cargo compatibility mode refused a workspace with more than 4096 manifests"
                    .to_owned(),
            );
        }
        let contents = fs::read_to_string(&canonical).map_err(|error| {
            format!(
                "age-policy: cannot read manifest {} while checking registry coverage: {error}",
                canonical.display()
            )
        })?;
        let value: toml::Value = toml::from_str(&contents).map_err(|error| {
            format!(
                "age-policy: cannot parse manifest {} while checking registry coverage: {error}",
                canonical.display()
            )
        })?;
        let base = canonical.parent().unwrap_or(Path::new("."));
        if workspace_dependencies.is_none()
            && let Some(dependencies) = value
                .get("workspace")
                .and_then(|workspace| workspace.get("dependencies"))
                .and_then(toml::Value::as_table)
        {
            workspace_dependencies = Some(WorkspaceDependencies {
                base: base.to_path_buf(),
                dependencies: dependencies.clone(),
            });
        }
        inspect_dependency_tables(&value, base, workspace_dependencies.as_ref(), &mut pending)?;
        enqueue_workspace_members(&value, base, &mut pending)?;
    }
    Ok(())
}

struct WorkspaceDependencies {
    base: PathBuf,
    dependencies: toml::map::Map<String, toml::Value>,
}

fn inspect_dependency_tables(
    value: &toml::Value,
    base: &Path,
    workspace_dependencies: Option<&WorkspaceDependencies>,
    pending: &mut Vec<PathBuf>,
) -> Result<()> {
    let Some(table) = value.as_table() else {
        return Ok(());
    };
    for (key, child) in table {
        if key == "workspace" {
            continue;
        }
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) && let Some(dependencies) = child.as_table()
        {
            for (name, specification) in dependencies {
                inspect_dependency(name, specification, base, workspace_dependencies, pending)?;
            }
            continue;
        }
        inspect_dependency_tables(child, base, workspace_dependencies, pending)?;
    }
    Ok(())
}

fn inspect_dependency(
    name: &str,
    specification: &toml::Value,
    base: &Path,
    workspace_dependencies: Option<&WorkspaceDependencies>,
    pending: &mut Vec<PathBuf>,
) -> Result<()> {
    let Some(specification) = specification.as_table() else {
        return Ok(());
    };
    let (specification, base) = if specification
        .get("workspace")
        .and_then(toml::Value::as_bool)
        == Some(true)
    {
        let workspace = workspace_dependencies.ok_or_else(|| {
            format!(
                "age-policy: dependency {name:?} inherits workspace configuration, but no workspace dependency definition was found"
            )
        })?;
        let inherited = workspace.dependencies.get(name).ok_or_else(|| {
            format!(
                "age-policy: dependency {name:?} inherits a missing workspace dependency definition"
            )
        })?;
        let Some(inherited) = inherited.as_table() else {
            return Ok(());
        };
        (inherited, workspace.base.as_path())
    } else {
        (specification, base)
    };

    if specification
        .get("registry")
        .and_then(toml::Value::as_str)
        .is_some_and(|registry| registry != "crates-io")
    {
        return Err(format!(
            "age-policy: old-Cargo compatibility mode cannot guarantee minimum age for dependency {name:?} from an alternate registry; use Cargo 1.100+"
        ));
    }
    if let Some(path) = specification.get("path").and_then(toml::Value::as_str) {
        pending.push(base.join(path).join("Cargo.toml"));
    }
    Ok(())
}

fn enqueue_workspace_members(
    value: &toml::Value,
    base: &Path,
    pending: &mut Vec<PathBuf>,
) -> Result<()> {
    let Some(members) = value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
    else {
        return Ok(());
    };
    for member in members {
        let pattern = member.as_str().ok_or_else(|| {
            "age-policy: workspace member pattern is not a string; cannot verify registry coverage"
                .to_owned()
        })?;
        expand_member_pattern(base, pattern, pending)?;
    }
    Ok(())
}

fn expand_member_pattern(base: &Path, pattern: &str, pending: &mut Vec<PathBuf>) -> Result<()> {
    if pattern.contains("**") || pattern.contains(['?', '[', ']']) {
        return Err(format!(
            "age-policy: old-Cargo compatibility mode cannot safely expand workspace member pattern {pattern:?}; use Cargo 1.100+"
        ));
    }
    let parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let mut paths = vec![base.to_path_buf()];
    for part in parts {
        let mut next = Vec::new();
        if part == "*" {
            for path in paths {
                let entries = fs::read_dir(&path).map_err(|error| {
                    format!(
                        "age-policy: cannot expand workspace member beneath {}: {error}",
                        path.display()
                    )
                })?;
                for entry in entries {
                    let entry = entry.map_err(|error| {
                        format!("age-policy: cannot inspect workspace member: {error}")
                    })?;
                    if entry
                        .file_type()
                        .map_err(|error| {
                            format!("age-policy: cannot inspect workspace member: {error}")
                        })?
                        .is_dir()
                    {
                        next.push(entry.path());
                    }
                }
            }
        } else if part.contains('*') {
            return Err(format!(
                "age-policy: old-Cargo compatibility mode supports only complete `*` workspace path components, not {part:?}; use Cargo 1.100+"
            ));
        } else {
            next.extend(paths.into_iter().map(|path| path.join(part)));
        }
        paths = next;
    }
    pending.extend(
        paths
            .into_iter()
            .map(|path| path.join("Cargo.toml"))
            .filter(|path| path.is_file()),
    );
    Ok(())
}

fn find_lockfile(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let start = manifest_path(args, cwd)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
    start
        .ancestors()
        .map(|directory| directory.join("Cargo.lock"))
        .find(|path| path.is_file())
}

fn find_manifest(args: &[String], cwd: &Path) -> Option<PathBuf> {
    manifest_path(args, cwd).or_else(|| {
        cwd.ancestors()
            .map(|directory| directory.join("Cargo.toml"))
            .find(|path| path.is_file())
    })
}

fn manifest_path(args: &[String], cwd: &Path) -> Option<PathBuf> {
    for (index, argument) in args.iter().enumerate() {
        let value = if argument == "--manifest-path" {
            args.get(index + 1).map(String::as_str)
        } else {
            argument.strip_prefix("--manifest-path=")
        };
        if let Some(value) = value {
            let path = PathBuf::from(value);
            return Some(if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            });
        }
    }
    None
}

fn parse_rfc3339(value: &str) -> Result<i128> {
    if value.len() < 20 {
        return Err(format!("invalid crates.io publication timestamp {value:?}"));
    }
    let year = decimal(&value[0..4])?;
    let month = decimal(&value[5..7])?;
    let day = decimal(&value[8..10])?;
    let hour = decimal(&value[11..13])?;
    let minute = decimal(&value[14..16])?;
    let second = decimal(&value[17..19])?;
    if &value[4..5] != "-"
        || &value[7..8] != "-"
        || &value[10..11] != "T"
        || &value[13..14] != ":"
        || &value[16..17] != ":"
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(format!("invalid crates.io publication timestamp {value:?}"));
    }

    let mut suffix = &value[19..];
    if let Some(fraction) = suffix.strip_prefix('.') {
        let end = fraction
            .find(['Z', '+', '-'])
            .ok_or_else(|| format!("invalid crates.io publication timestamp {value:?}"))?;
        if end == 0 || !fraction[..end].bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("invalid crates.io publication timestamp {value:?}"));
        }
        suffix = &fraction[end..];
    }
    let offset = if suffix == "Z" {
        0_i64
    } else if suffix.len() == 6 && matches!(&suffix[0..1], "+" | "-") && &suffix[3..4] == ":" {
        let sign = if &suffix[0..1] == "+" { 1 } else { -1 };
        let hours = decimal(&suffix[1..3])?;
        let minutes = decimal(&suffix[4..6])?;
        if hours > 23 || minutes > 59 {
            return Err(format!("invalid crates.io publication timestamp {value:?}"));
        }
        sign * (hours * 3_600 + minutes * 60)
    } else {
        return Err(format!("invalid crates.io publication timestamp {value:?}"));
    };

    let days = days_from_civil(year, month, day);
    Ok(i128::from(
        days * 86_400 + hour * 3_600 + minute * 60 + second - offset,
    ))
}

fn decimal(value: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| format!("invalid decimal field in publication timestamp: {value:?}"))
}

fn days_from_civil(mut year: i64, month: i64, day: i64) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::process::{Command, Output};
    use std::sync::atomic::AtomicU64;

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cargo-wrapper-age-policy-registry-test-{}-{nonce}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct MockRegistry {
        address: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        errors: Arc<Mutex<Vec<String>>>,
        downloads: Arc<Mutex<Vec<String>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl MockRegistry {
        fn start(index: Vec<u8>, archives: BTreeMap<String, Vec<u8>>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let errors = Arc::new(Mutex::new(Vec::new()));
            let downloads = Arc::new(Mutex::new(Vec::new()));
            let worker_stop = Arc::clone(&stop);
            let worker_errors = Arc::clone(&errors);
            let worker_downloads = Arc::clone(&downloads);
            let thread = thread::spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = handle_mock_registry_request(
                                stream,
                                address,
                                &index,
                                &archives,
                                &worker_downloads,
                            ) && let Ok(mut values) = worker_errors.lock()
                            {
                                values.push(error);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => {
                            worker_errors
                                .lock()
                                .unwrap()
                                .push(format!("mock registry accept failed: {error}"));
                            break;
                        }
                    }
                }
            });
            Self {
                address,
                stop,
                errors,
                downloads,
                thread: Some(thread),
            }
        }

        fn sparse_url(&self) -> String {
            format!("sparse+http://{}/", self.address)
        }

        fn finish(mut self) -> Vec<String> {
            self.stop.store(true, Ordering::Release);
            let _ =
                TcpStream::connect(self.address).and_then(|stream| stream.shutdown(Shutdown::Both));
            self.thread.take().unwrap().join().unwrap();
            let errors = self.errors.lock().unwrap();
            assert!(errors.is_empty(), "mock registry errors: {errors:?}");
            self.downloads.lock().unwrap().clone()
        }
    }

    impl Drop for MockRegistry {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ =
                TcpStream::connect(self.address).and_then(|stream| stream.shutdown(Shutdown::Both));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn handle_mock_registry_request(
        mut stream: TcpStream,
        address: std::net::SocketAddr,
        index: &[u8],
        archives: &BTreeMap<String, Vec<u8>>,
        downloads: &Mutex<Vec<String>>,
    ) -> Result<()> {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| error.to_string())?;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Ok(());
            }
            request.extend_from_slice(&buffer[..count]);
            if request.len() > MAX_REQUEST_HEADERS {
                return Err("mock registry request headers were too large".to_owned());
            }
        }
        let request = std::str::from_utf8(&request).map_err(|error| error.to_string())?;
        let mut request_parts = request
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let method = request_parts.next().unwrap_or_default();
        let path = request_parts
            .next()
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default();

        let (content_type, body) = if path == "/config.json" {
            (
                "application/json",
                format!(
                    "{{\"dl\":\"http://{address}/api/v1/crates\",\"api\":\"http://{address}\"}}"
                )
                .into_bytes(),
            )
        } else if path == "/ag/e-/age-policy-demo" {
            ("text/plain", index.to_vec())
        } else if let Some(version) = path
            .strip_prefix("/api/v1/crates/age-policy-demo/")
            .and_then(|value| value.strip_suffix("/download"))
        {
            let archive = archives
                .get(version)
                .ok_or_else(|| format!("mock registry has no archive for {version}"))?;
            downloads.lock().unwrap().push(version.to_owned());
            ("application/octet-stream", archive.clone())
        } else {
            return Err(format!("unexpected mock registry request: {method} {path}"));
        };

        if method == "HEAD" {
            write_response(&mut stream, 200, content_type, b"")
        } else if method == "GET" {
            write_response(&mut stream, 200, content_type, &body)
        } else {
            Err(format!("unexpected mock registry method: {method}"))
        }
    }

    fn test_cargo() -> OsString {
        std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
    }

    fn assert_command_succeeded(output: &Output) {
        assert!(
            output.status.success(),
            "command failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn package_mock_crate(root: &Path, version: &str) -> Vec<u8> {
        let source = root.join(format!("package-{version}"));
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(
            source.join("Cargo.toml"),
            format!(
                "[package]\nname = \"age-policy-demo\"\nversion = \"{version}\"\nedition = \"2021\"\nlicense = \"MIT\"\ndescription = \"Harmless mock crate for the age-policy test\"\n"
            ),
        )
        .unwrap();
        fs::write(
            source.join("src/lib.rs"),
            format!("pub const SELECTED_VERSION: &str = \"{version}\";\n"),
        )
        .unwrap();
        let output = Command::new(test_cargo())
            .args(["package", "--allow-dirty", "--no-verify"])
            .current_dir(&source)
            .env("CARGO_HOME", root.join("packager-cargo-home"))
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .unwrap();
        assert_command_succeeded(&output);
        fs::read(
            source
                .join("target/package")
                .join(format!("age-policy-demo-{version}.crate")),
        )
        .unwrap()
    }

    fn checksum(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn parses_cargo_index_timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01:00").unwrap(), 0);
        assert_eq!(
            parse_rfc3339("2026-09-05T12:34:56.123Z").unwrap(),
            1_788_611_696
        );
    }

    #[test]
    fn unused_workspace_path_dependency_does_not_block_compatibility_mode() {
        let directory = TestDirectory::new();
        let member = directory.path().join("member");
        fs::create_dir(&member).unwrap();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n\
             \n[workspace.dependencies]\nunused = { path = \"missing\" }\n",
        )
        .unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        validate_manifest_registries(&["build".to_owned()], directory.path()).unwrap();
    }

    #[test]
    fn inherited_workspace_path_dependency_is_still_checked_when_used() {
        let directory = TestDirectory::new();
        let member = directory.path().join("member");
        fs::create_dir(&member).unwrap();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n\
             \n[workspace.dependencies]\nrequired = { path = \"missing\" }\n",
        )
        .unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             \n[dependencies]\nrequired = { workspace = true }\n",
        )
        .unwrap();

        let error =
            validate_manifest_registries(&["build".to_owned()], directory.path()).unwrap_err();
        assert!(error.contains("cannot resolve manifest"));
        assert!(error.contains("missing/Cargo.toml"));
    }

    #[test]
    fn filters_too_new_but_preserves_locked_and_missing_pubtime() {
        let body = br#"{"name":"demo","vers":"1.0.0","pubtime":"1970-01-01T00:00:00Z"}
{"name":"demo","vers":"1.1.0","pubtime":"2026-09-05T12:34:56Z"}
{"name":"demo","vers":"1.2.0"}
"#;
        let filter = Filter {
            cutoff: 1,
            locked: Arc::new(HashSet::from([("demo".to_owned(), "1.1.0".to_owned())])),
        };
        let output = String::from_utf8(filter_index(body, &filter).unwrap()).unwrap();
        assert!(output.contains("1.0.0"));
        assert!(output.contains("1.1.0"));
        assert!(output.contains("1.2.0"));
    }

    #[test]
    fn mock_registry_selects_old_enough_version_instead_of_29_day_release() {
        let directory = TestDirectory::new();
        let old_archive = package_mock_crate(directory.path(), "0.0.1");
        let young_archive = package_mock_crate(directory.path(), "0.0.2");
        let unfiltered = format!(
            "{{\"name\":\"age-policy-demo\",\"vers\":\"0.0.1\",\"deps\":[],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false,\"pubtime\":\"2026-01-01T00:00:00Z\"}}\n\
             {{\"name\":\"age-policy-demo\",\"vers\":\"0.0.2\",\"deps\":[],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false,\"pubtime\":\"2026-01-03T00:00:00Z\"}}\n",
            checksum(&old_archive),
            checksum(&young_archive)
        );
        let filter = Filter {
            cutoff: parse_rfc3339("2026-01-02T00:00:00Z").unwrap(),
            locked: Arc::new(HashSet::new()),
        };
        let filtered = filter_index(unfiltered.as_bytes(), &filter).unwrap();
        let filtered_text = std::str::from_utf8(&filtered).unwrap();
        assert!(filtered_text.contains("\"vers\":\"0.0.1\""));
        assert!(!filtered_text.contains("\"vers\":\"0.0.2\""));

        println!(
            "\n============================================================\n\
             30-DAY AGE POLICY MOCK REGISTRY DEMO\n\
             ============================================================\n\
             Requested: newest compatible age-policy-demo release\n\
             Registry latest: v0.0.2, published 29 days ago\n\
             Policy action: withheld v0.0.2 because it is younger than 30 days\n\
             Eligible fallback: v0.0.1, published 31 days ago\n"
        );

        let registry = MockRegistry::start(
            filtered,
            BTreeMap::from([
                ("0.0.1".to_owned(), old_archive),
                ("0.0.2".to_owned(), young_archive),
            ]),
        );
        let project = directory.path().join("consumer");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"age-policy-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             \n[dependencies]\nage-policy-demo = \">=0.0.1, <0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            project.join("src/main.rs"),
            "fn main() { println!(\"{}\", age_policy_demo::SELECTED_VERSION); }\n",
        )
        .unwrap();
        let output = Command::new(test_cargo())
            .arg("--config")
            .arg("source.crates-io.replace-with=\"age-policy-test\"")
            .arg("--config")
            .arg(format!(
                "source.age-policy-test.registry=\"{}\"",
                registry.sparse_url()
            ))
            .args(["check", "--manifest-path"])
            .arg(project.join("Cargo.toml"))
            .env("CARGO_HOME", directory.path().join("consumer-cargo-home"))
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .unwrap();
        println!("Cargo output:\n{}", String::from_utf8_lossy(&output.stderr));
        assert_command_succeeded(&output);

        let lock = fs::read_to_string(project.join("Cargo.lock")).unwrap();
        assert!(lock.contains("name = \"age-policy-demo\"\nversion = \"0.0.1\""));
        assert!(!lock.contains("name = \"age-policy-demo\"\nversion = \"0.0.2\""));
        let downloads = registry.finish();
        assert_eq!(downloads, ["0.0.1"]);
        println!(
            "Selected and compiled by Cargo: v0.0.1\n\
             PASS: the 29-day v0.0.2 archive was never downloaded or executed."
        );
    }
}
