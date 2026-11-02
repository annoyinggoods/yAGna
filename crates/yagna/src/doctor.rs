//! The `doctor` verb: one environment probe, reported line by line.
//!
//! Checks, in order: the GitHub token, the API rate limit, the data
//! directory, the database, the asset-cache directory, and a versions
//! line. Each check prints one line — `[ok]` or `[fail]` — and any fail
//! exits 1. Nothing is repaired: creating a missing directory is part of
//! proving it creatable, and every problem line names the command or
//! setting that would fix it. The token is never printed.
//!
//! Items: `run` — the verb; `check_rate_limit`, `check_dir`,
//! `check_database`, `check_cache_size`, `check_versions` — one probe
//! each.
//! Depends on: yagna_forge (the rate-limit probe goes through the forge
//! facade), yagna_store (the database door), directories (the data
//! locations), crate::token and crate::paths (the shared edges).
//! Called by: crate::main (the `doctor` verb).

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use yagna_forge::ForgeHost;

/// Runs every check in order and exits 1 when any failed.
pub fn run() -> ExitCode {
    let token = crate::token::acquire(true);
    let mut failures = usize::from(token.is_none());

    let host = match ForgeHost::new() {
        Ok(host) => host,
        Err(error) => {
            println!("[fail] async runtime: {error}");
            return finish(failures + 1);
        }
    };
    report(check_rate_limit(&host, token.as_deref()), &mut failures);

    let dirs = match crate::paths::project_dirs() {
        Ok(dirs) => dirs,
        Err(problem) => {
            println!("[fail] data locations: {problem}");
            return finish(failures + 1);
        }
    };
    report(check_dir("data directory", dirs.data_dir()), &mut failures);
    report(check_database(&crate::paths::db_path(&dirs)), &mut failures);
    report(check_dir("asset cache", dirs.cache_dir()), &mut failures);
    let (settings, warning) = crate::settings::load();
    if let Some(line) = warning {
        println!("{line}");
    }
    report(
        check_cache_size(dirs.cache_dir(), settings.cache_cap_bytes),
        &mut failures,
    );
    report(check_versions(), &mut failures);
    finish(failures)
}

/// Prints one check's line, counting failures. Returns the detail when
/// the check passed.
fn report(check: Result<String, String>, failures: &mut usize) -> Option<String> {
    match check {
        Ok(detail) => {
            println!("[ok] {detail}");
            Some(detail)
        }
        Err(problem) => {
            println!("[fail] {problem}");
            *failures += 1;
            None
        }
    }
}

/// The exit after the checks: quiet success, or a loud count.
fn finish(failures: usize) -> ExitCode {
    if failures == 0 {
        return ExitCode::SUCCESS;
    }
    println!("doctor: {failures} check(s) failed");
    ExitCode::FAILURE
}

/// The API probe: one free rate-limit request through the forge facade,
/// reporting the remaining budgets.
fn check_rate_limit(host: &ForgeHost, token: Option<&str>) -> Result<String, String> {
    let limits = host
        .rate_limit(token)
        .map_err(|error| format!("GitHub API unreachable: {}", error.describe()))?;
    Ok(format!(
        "GitHub API reachable — core {} of {} requests left this hour, \
         search {} of {} left this minute",
        limits.core_remaining, limits.core_limit, limits.search_remaining, limits.search_limit
    ))
}

/// The directory probe: resolves, can be created, and accepts a file.
/// Creating a missing directory is the creatable half of the check.
fn check_dir(label: &str, path: &Path) -> Result<String, String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("{label} {}: cannot be created: {error}", path.display()))?;
    let probe = path.join("yagna-doctor-probe.tmp");
    fs::write(&probe, b"probe")
        .map_err(|error| format!("{label} {}: not writable: {error}", path.display()))?;
    let _ = fs::remove_file(&probe);
    Ok(format!("{label}: {}", path.display()))
}

/// The database probe: opens the catalog through the store facade and
/// reports its schema version. An absent file is created empty.
fn check_database(path: &Path) -> Result<String, String> {
    let store = yagna_store::Store::open(path)
        .map_err(|error| format!("database at {}: {error}", path.display()))?;
    let version = store
        .schema_version()
        .map_err(|error| format!("database at {}: {error}", path.display()))?;
    Ok(format!(
        "database open at {} (schema version {version})",
        path.display()
    ))
}

/// The cache-size probe: what the directory holds against the
/// settings' cap. Informational — a cache over its cap is not a
/// failure, it is what the next launch's prune is for.
fn check_cache_size(dir: &Path, cap_bytes: u64) -> Result<String, String> {
    let stats = yagna_store::cache_stats(dir);
    Ok(format!(
        "cache holds {} in {} file(s) — cap {}",
        crate::paths::human_bytes(stats.bytes),
        stats.files,
        crate::paths::human_bytes(cap_bytes),
    ))
}

/// The versions line: this binary and the pinned UI framework.
fn check_versions() -> Result<String, String> {
    Ok(format!(
        "yagna {}, gpui {}",
        env!("CARGO_PKG_VERSION"),
        env!("YAGNA_GPUI_PIN"),
    ))
}
