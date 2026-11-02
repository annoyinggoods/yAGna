//! The digest generation pipeline: one claimed `digest` queue item's
//! tarball fetch, spec filter, formatting and storage. The tab's
//! worker and the CLI's enrich runner both work through this one
//! implementation — two callers, one pipeline.
//!
//! The spec is the standard one (the default excludes and the default
//! per-file cap); the branch is the item's default branch — a
//! repository without one has nothing to digest.
//!
//! Items: `generate` — the pipeline; `Failure` — what a failed
//! generation means to the caller (absent, rate-limited, transient,
//! broken); `Outcome` — the caller-shaped verdicts.
//! Depends on: yagna_store (the queue item, the storage verb),
//! yagna_forge (the tarball endpoint), yagna_core (the digest math).
//! Called by: crate::enrich (the CLI runner) and crate::ui::workers
//! (the tab's Generate action).

use std::time::Duration;

use chrono::{DateTime, Utc};
use yagna_core::digest::{self, DigestInput};
use yagna_core::model::{Forge as ForgeKind, RepoRef};
use yagna_forge::{ForgeError, ForgeHost, GithubForge, rate_pause};
use yagna_store::{QueueItem, Store};

/// Runs the pipeline for one claimed item: fetch, filter, format,
/// store. The queue row is the caller's to stamp — the failure kind
/// tells it how.
pub(crate) fn generate(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    item: &QueueItem,
    now: DateTime<Utc>,
) -> Result<(), Failure> {
    let Some(branch) = item.default_branch.as_deref() else {
        return Err(Failure::Absent(
            "no default branch — nothing to digest".to_string(),
        ));
    };
    let repo = RepoRef {
        forge: ForgeKind::GitHub,
        owner: item.owner.clone(),
        name: item.name.clone(),
    };
    let tarball = host.tarball(forge, &repo, branch);
    let input = match tarball {
        Ok(input) => input,
        Err(error) => match rate_pause(&error) {
            Some(pause) => {
                return Err(Failure::RateLimited {
                    reason: error.describe(),
                    pause,
                });
            }
            None if matches!(error, ForgeError::Absent(_)) => {
                return Err(Failure::Absent(format!(
                    "{} does not exist on this repository",
                    absent_what(&error)
                )));
            }
            None => return Err(Failure::Transient(error.describe())),
        },
    };
    let spec = digest::standard_spec(&repo);
    let files = digest::filter_files(input.files, &spec);
    let filtered = DigestInput {
        repo: input.repo,
        files,
        truncated_by: input.truncated_by,
        taken_commit: input.taken_commit,
    };
    store
        .store_digest(item.repo_rowid, &spec, &filtered, now)
        .map_err(|problem| Failure::Broken(format!("catalog: {problem}")))
}

/// The absent thing's name, for the message.
fn absent_what(error: &ForgeError) -> String {
    match error {
        ForgeError::Absent(what) => what.to_string(),
        _ => "the tarball".to_string(),
    }
}

/// What a failed generation means. `Absent` is the repository's own
/// shape (no branch, no tarball); `RateLimited` carries the named wait
/// and `Transient` may succeed on another attempt; `Broken` is the
/// catalog's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    Absent(String),
    RateLimited { reason: String, pause: Duration },
    Transient(String),
    Broken(String),
}

impl Failure {
    /// The user-facing reason, in one line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Failure::Absent(reason)
            | Failure::RateLimited { reason, .. }
            | Failure::Transient(reason)
            | Failure::Broken(reason) => reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_describe_their_reason() {
        let absent = Failure::Absent("no default branch".to_string());
        assert_eq!(absent.describe(), "no default branch");
        let limited = Failure::RateLimited {
            reason: "slow down".to_string(),
            pause: std::time::Duration::from_secs(30),
        };
        assert_eq!(limited.describe(), "slow down");
    }
}
