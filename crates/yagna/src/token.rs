//! The GitHub token the CLI verbs share: one probe order, one masked
//! line, one return path.
//!
//! `acquire` looks in `GITHUB_TOKEN` first, then asks `gh auth token`.
//! The value is never printed, never logged, and never part of an error
//! string — the caller gets it to hand to the forge facade, where it
//! travels in request headers only.
//!
//! Items: `acquire` — the probe; `gh_auth_token` — the fallback.
//! Depends on: nothing outside std.
//! Called by: crate::doctor, crate::sync and crate::prefetch.

/// The token probe: `GITHUB_TOKEN` first, then `gh auth token`. Prints
/// one line when `report` (the doctor always reports; a verb in quiet
/// mode does not) — the value is never shown — and returns the trimmed
/// value, or none when no token is found.
pub(crate) fn acquire(report: bool) -> Option<String> {
    if let Ok(value) = std::env::var("GITHUB_TOKEN")
        && !value.trim().is_empty()
    {
        if report {
            println!("[ok] GitHub token present (from GITHUB_TOKEN; value not shown)");
        }
        return Some(value.trim().to_string());
    }
    match gh_auth_token() {
        Ok(value) => {
            if report {
                println!("[ok] GitHub token present (from `gh auth token`; value not shown)");
            }
            Some(value)
        }
        Err(_) => {
            if report {
                println!(
                    "[fail] GitHub token: none found — set GITHUB_TOKEN or run `gh auth login`"
                );
            }
            None
        }
    }
}

/// Asks the `gh` CLI for its token, trimmed.
fn gh_auth_token() -> Result<String, String> {
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| "`gh` is not available".to_string())?;
    if !output.status.success() {
        return Err("`gh auth token` failed".to_string());
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err("`gh auth token` returned nothing".to_string());
    }
    Ok(token)
}
