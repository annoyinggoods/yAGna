//! The network world of yAGna: the one async runtime, the forge seam,
//! the sweep job that streams a sweep's pages out of it, and the
//! per-repository enrichment endpoints (README, license text, tree).
//!
//! `Forge` is the seam every network call goes through; `GithubForge` is
//! its GitHub arm (the `github` module, which also owns the rate-limit
//! classifiers and the tarball streaming). `ForgeHost` owns the
//! process's single runtime — nothing else in the workspace builds one
//! — and hands sweeps out as `SweepJob`s the caller awaits from its own
//! executor; the enrichment endpoints block on the same runtime for
//! the CLI runner. No UI, no database — a gate test keeps them out.
//!
//! Items: `Forge` — the seam (`search` and `user_starred` are the
//! discovery pair, `following` the people source); `StarredPage` —
//! one starred-sweep page and its cursor; `SweepPage` — what a running
//! sweep delivers, numbered; `SweepJob` — the handle one sweep is
//! awaited through; `ForgeError` — the crate's errors (`Absent` marks
//! a thing that legitimately does not exist); `ForgeHost` — the runtime
//! owner and facade (`rate_limit` — the free probe, `jitter_delay` —
//! the serial politeness pause, `search`, `user_starred` and
//! `following` — the endpoints driven to completion);
//! `SEARCH_DEBOUNCE_SECS` — the search tab's floor between queries;
//! `rate_pause` (re-export) — the rate-limit classifier the runners
//! ask.
//! Depends on: octocrab (via the github module), tokio (the runtime and
//! the page channel), yagna_core (the DTOs crossing the seam).
//! Called by: the app crate (the doctor, sync and enrich paths).

mod github;

pub use github::{GithubForge, rate_pause};

use std::future::Future;

use chrono::{DateTime, Utc};
use yagna_core::digest::DigestInput;
use yagna_core::model::{
    DiscoverQuery, RateLimitReport, RepoLiveFacts, RepoRef, RepoSnapshot, TreeEntry,
};

/// One page of the starred sweep: the repositories seen, the cursor
/// that fetches the next page — none once the sweep is exhausted — and
/// the page's remaining rate-limit budget with its reset instant, when
/// the page carried them.
#[derive(Debug)]
pub struct StarredPage {
    pub repos: Vec<RepoSnapshot>,
    pub cursor: Option<String>,
    pub rate_remaining: Option<u64>,
    pub rate_reset: Option<DateTime<Utc>>,
}

/// The seam every network call goes through. Inputs are refs into the
/// core's DTOs, outputs are core DTOs — nothing here leaks client types.
/// The futures are returned RPITIT-style: no boxing, no new dependency,
/// no trait objects — a sweep runs on one concrete forge, built on the
/// host's runtime.
pub trait Forge: Send + Sync {
    /// The identifier the store keeps in its forge column.
    fn id(&self) -> &'static str;

    /// One page of the starred sweep; a `None` cursor starts it. The
    /// implementation is responsible for pausing on rate limits and
    /// transient transport failures.
    fn starred_page(
        &self,
        cursor: Option<String>,
    ) -> impl Future<Output = Result<StarredPage, ForgeError>> + Send;

    /// The full sweep: every starred repository, the pages folded
    /// together. Consumers that want the pages as they land drive
    /// [`Forge::starred_page`] themselves (the host's sweep job does).
    fn starred(&self) -> impl Future<Output = Result<Vec<RepoSnapshot>, ForgeError>> + Send {
        async move {
            let mut all = Vec::new();
            let mut cursor = None;
            loop {
                let page = self.starred_page(cursor).await?;
                all.extend(page.repos);
                let Some(next) = page.cursor else {
                    return Ok(all);
                };
                cursor = Some(next);
            }
        }
    }

    /// One repository's live facts in a single call: the snapshot
    /// fields the catalog keeps, plus the open-issue count only the
    /// live view shows. A repository that is not there is
    /// [`ForgeError::Absent`].
    fn repository(
        &self,
        repo: &RepoRef,
    ) -> impl Future<Output = Result<RepoLiveFacts, ForgeError>> + Send;

    /// The repository's README, decoded. The endpoint resolves filename
    /// variants and branch itself; a 404 falls back to the explicit
    /// variant list over the contents endpoint, and when nothing answers
    /// the error is [`ForgeError::Absent`] — the repository has no
    /// README at all.
    fn readme(&self, repo: &RepoRef) -> impl Future<Output = Result<String, ForgeError>> + Send;
    /// The license file's text, or `Ok(None)` when the repository has no
    /// license file at all — the `LICENSE*` first match in the root
    /// listing.
    fn license_text(
        &self,
        repo: &RepoRef,
    ) -> impl Future<Output = Result<Option<String>, ForgeError>> + Send;

    /// The repository's full tree at `branch`, recursive, with the
    /// service's truncation flag carried — never swallowed.
    fn tree(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> impl Future<Output = Result<(Vec<TreeEntry>, bool), ForgeError>> + Send;

    /// One asset's bytes — the image a README points at. The bytes are
    /// capped: an asset past the cap is refused rather than fetched.
    /// The request is anonymous: a README can point at any host, and
    /// the user's token never travels there.
    fn fetch_asset(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, ForgeError>> + Send;

    /// The repository's exact first commit: the oldest commit on its
    /// default branch, and `Ok(None)` when it has no commits at all.
    ///
    /// The endpoint's page carries the last page's link, so the oldest
    /// commit is two requests away however deep the history is — the
    /// pages are never walked.
    fn first_commit(
        &self,
        repo: &RepoRef,
    ) -> impl Future<Output = Result<Option<DateTime<Utc>>, ForgeError>> + Send;

    /// The repository's tarball at `branch`, streamed and decoded
    /// straight into the digest formatter's input — no clone on disk,
    /// no temp checkout. The caps are enforced mid-stream: a walk past
    /// the total-bytes or file-count cap stops, and the returned input
    /// carries which cap stopped it.
    fn tarball(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> impl Future<Output = Result<DigestInput, ForgeError>> + Send;

    /// The DISCOVER query against the search API: one query per
    /// interaction, at most `SEARCH_DEBOUNCE_SECS` apart — the search
    /// endpoint's 30 req/min budget is respected by construction, and
    /// the runner shows the remaining budget beside the controls.
    fn search(
        &self,
        query: &DiscoverQuery,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<RepoSnapshot>, ForgeError>> + Send;

    /// Another user's public starred list: the read-only shadow source.
    /// Stored scoped by login, never mixed into the catalog.
    fn user_starred(
        &self,
        login: &str,
    ) -> impl Future<Output = Result<Vec<RepoSnapshot>, ForgeError>> + Send;

    /// The authenticated user's followed logins — the suggestion
    /// source for the PEOPLE surface. The endpoint answers with the
    /// people the token's own user follows; an empty list is a valid
    /// answer, and a forge without the concept answers with one too.
    fn following(&self) -> impl Future<Output = Result<Vec<String>, ForgeError>> + Send;
}

/// The search tab's floor between two queries: the 30 req/min budget
/// is one query per two seconds; three seconds keeps normal
/// interaction under it with margin.
pub const SEARCH_DEBOUNCE_SECS: u64 = 3;

/// One page a running sweep delivers: the page's position in the sweep
/// (1-based), its repositories, the cursor the sweep follows next —
/// none after the last page — and the rate-limit budget the page
/// reported, for the runner's visible pause.
pub struct SweepPage {
    pub number: u32,
    pub repos: Vec<RepoSnapshot>,
    pub cursor: Option<String>,
    pub rate_remaining: Option<u64>,
    pub rate_reset: Option<DateTime<Utc>>,
}

/// The handle one running sweep is awaited through. Pages arrive one at
/// a time — the next is fetched only after the previous was taken, with
/// a small jitter in between. Dropping the job abandons the sweep; the
/// runtime task notices the closed channel and stops.
pub struct SweepJob {
    pages: tokio::sync::mpsc::Receiver<Result<SweepPage, ForgeError>>,
}

impl SweepJob {
    /// The next page of the sweep: some until the sweep is exhausted or
    /// fails, then none. A failed page is the sweep's last word.
    pub async fn next_page(&mut self) -> Option<Result<SweepPage, ForgeError>> {
        self.pages.recv().await
    }
}

/// The errors the forge can raise.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("the async runtime failed to start: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("github request failed: {0}")]
    Github(#[from] octocrab::Error),
    #[error("the sweep response failed to parse: {0}")]
    Parse(String),
    #[error("{0} does not exist on this repository")]
    Absent(&'static str),
    #[error("the asset is larger than the {limit} byte cap")]
    TooLarge { limit: usize },
    #[error("the asset could not be read: {0}")]
    Asset(String),
}

impl ForgeError {
    /// The user-facing description of this error. The GitHub variant's own
    /// display is just the variant name — the service's message and status
    /// sit one level down, and they are the part that says what went wrong.
    pub fn describe(&self) -> String {
        let ForgeError::Github(error) = self else {
            return format!("{self}");
        };
        let octocrab::Error::GitHub { source, .. } = error else {
            return format!("{error}");
        };
        format!("GitHub answered {}: {}", source.status_code, source.message)
    }
}

/// The owner of the one async runtime and the facade for every network
/// call. UI consumers spawn work through it and collect results over
/// channels; the CLI verbs drive futures to completion.
pub struct ForgeHost {
    runtime: tokio::runtime::Runtime,
}

/// The pause between two sweep-page fetches: the floor and the spread of
/// the jitter, in milliseconds.
const JITTER_FLOOR_MILLIS: u64 = 200;
const JITTER_SPREAD_MILLIS: u64 = 500;

impl ForgeHost {
    /// Creates the runtime. One host per process; never two.
    pub fn new() -> Result<Self, ForgeError> {
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
        })
    }

    /// Drives one future to completion on the owned runtime. The CLI
    /// verbs use this; a UI thread must not — it spawns instead.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// One rate-limit probe: a single request for the free `/rate_limit`
    /// endpoint. `token` authenticates the probe when present; the value
    /// travels in a request header only and is never part of an error.
    pub fn rate_limit(&self, token: Option<&str>) -> Result<RateLimitReport, ForgeError> {
        self.block_on(async { rate_limit_once(token).await })
    }

    /// The full starred sweep of the GitHub forge as one list — the
    /// simple facade form. `sync` drives the same machinery through
    /// [`ForgeHost::sweep`] instead, to store every page as it lands.
    pub fn starred(&self, token: &str) -> Result<Vec<RepoSnapshot>, ForgeError> {
        self.block_on(async {
            let forge = GithubForge::new(token)?;
            forge.starred().await
        })
    }

    /// Starts a starred sweep and returns the job its pages arrive
    /// through. `make_forge` builds the forge to sweep with — it runs on
    /// the owned runtime, because client construction is runtime work
    /// (the client's background middleware spawns a worker at build
    /// time). A construction failure is the job's first event. The
    /// consumer can sit on any executor: the channel's receive end is
    /// runtime-free.
    pub fn sweep<F, M>(&self, make_forge: M) -> SweepJob
    where
        F: Forge + 'static,
        M: FnOnce() -> Result<F, ForgeError> + Send + 'static,
    {
        let (sender, pages) = tokio::sync::mpsc::channel(1);
        self.runtime.spawn(async move {
            let forge = match make_forge() {
                Ok(forge) => forge,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            drive_sweep(forge, sender).await;
        });
        SweepJob { pages }
    }

    /// Builds one forge on the owned runtime — client construction is
    /// runtime work. One forge serves a whole enrichment run; the
    /// blocking wrappers below take it by reference.
    pub fn forge<F: Forge>(
        &self,
        make: impl FnOnce() -> Result<F, ForgeError>,
    ) -> Result<F, ForgeError> {
        self.block_on(async { make() })
    }

    /// The repository's README, driven to completion on the runtime.
    pub fn readme<F: Forge>(&self, forge: &F, repo: &RepoRef) -> Result<String, ForgeError> {
        self.block_on(forge.readme(repo))
    }

    /// The license file's text (`None` when there is none), driven to
    /// completion on the runtime.
    pub fn license_text<F: Forge>(
        &self,
        forge: &F,
        repo: &RepoRef,
    ) -> Result<Option<String>, ForgeError> {
        self.block_on(forge.license_text(repo))
    }

    /// The recursive tree at `branch` with its truncation flag, driven
    /// to completion on the runtime.
    pub fn tree<F: Forge>(
        &self,
        forge: &F,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<(Vec<TreeEntry>, bool), ForgeError> {
        self.block_on(forge.tree(repo, branch))
    }

    /// One asset's bytes, driven to completion on the runtime.
    pub fn fetch_asset<F: Forge>(&self, forge: &F, url: &str) -> Result<Vec<u8>, ForgeError> {
        self.block_on(forge.fetch_asset(url))
    }

    /// The repository's exact first commit, driven to completion on the
    /// runtime.
    pub fn first_commit<F: Forge>(
        &self,
        forge: &F,
        repo: &RepoRef,
    ) -> Result<Option<DateTime<Utc>>, ForgeError> {
        self.block_on(forge.first_commit(repo))
    }

    /// The repository's tarball at `branch` as the digest input, driven
    /// to completion on the runtime.
    pub fn tarball<F: Forge>(
        &self,
        forge: &F,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<DigestInput, ForgeError> {
        self.block_on(forge.tarball(repo, branch))
    }

    /// One DISCOVER search, driven to completion on the runtime.
    pub fn search<F: Forge>(
        &self,
        forge: &F,
        query: &DiscoverQuery,
        now: DateTime<Utc>,
    ) -> Result<Vec<RepoSnapshot>, ForgeError> {
        self.block_on(forge.search(query, now))
    }

    /// One user's public stars, driven to completion on the runtime.
    pub fn user_starred<F: Forge>(
        &self,
        forge: &F,
        login: &str,
    ) -> Result<Vec<RepoSnapshot>, ForgeError> {
        self.block_on(forge.user_starred(login))
    }

    /// The authenticated user's followed logins, driven to completion
    /// on the runtime.
    pub fn following<F: Forge>(&self, forge: &F) -> Result<Vec<String>, ForgeError> {
        self.block_on(forge.following())
    }

    /// The pause serial fetch loops take between two items: a jittered
    /// delay, so bursts do not arrive with metronome regularity.
    pub fn jitter_delay(&self) -> std::time::Duration {
        jitter_delay()
    }
}

/// The sweep's page loop: fetch a page, hand it over, repeat — until the
/// sweep is exhausted, an error lands, or the consumer walks away.
async fn drive_sweep<F: Forge>(
    forge: F,
    sender: tokio::sync::mpsc::Sender<Result<SweepPage, ForgeError>>,
) {
    let mut cursor = None;
    let mut number = 0;
    loop {
        number += 1;
        let page = match forge.starred_page(cursor).await {
            Ok(page) => page,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        };
        let next = page.cursor.clone();
        let page = SweepPage {
            number,
            repos: page.repos,
            cursor: next.clone(),
            rate_remaining: page.rate_remaining,
            rate_reset: page.rate_reset,
        };
        let delivered = sender.send(Ok(page)).await;
        if delivered.is_err() {
            return;
        }
        let Some(next) = next else {
            return;
        };
        cursor = Some(next);
        tokio::time::sleep(jitter_delay()).await;
    }
}

/// The jitter between two page fetches: a floor plus a slice of the wall
/// clock's sub-second nanos, so consecutive pages do not arrive with
/// metronome regularity. Secondary rate limits bite rapid bursts even
/// under the primary one. The source is the clock, not a CSPRNG — the
/// goal is de-synchronization of our own requests, not unpredictability
/// to an adversary.
fn jitter_delay() -> std::time::Duration {
    use std::time::{Duration, UNIX_EPOCH};
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(JITTER_FLOOR_MILLIS + nanos % JITTER_SPREAD_MILLIS)
}

/// The rate-limit probe itself, run inside the runtime.
async fn rate_limit_once(token: Option<&str>) -> Result<RateLimitReport, ForgeError> {
    let builder = octocrab::Octocrab::builder();
    let client = match token {
        Some(present) => crate::github::tuned(builder)
            .personal_token(present.to_string())
            .build()?,
        None => crate::github::tuned(builder).build()?,
    };
    let limits = client.ratelimit().get().await?;
    // octocrab carries the reset as a unix timestamp; the report speaks
    // UTC instants.
    let core_reset = chrono::DateTime::from_timestamp(limits.resources.core.reset as i64, 0)
        .map(|instant| instant.with_timezone(&chrono::Utc));
    Ok(RateLimitReport {
        core_limit: limits.resources.core.limit as u64,
        core_remaining: limits.resources.core.remaining as u64,
        search_limit: limits.resources.search.limit as u64,
        search_remaining: limits.resources.search.remaining as u64,
        core_reset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagna_core::model::Forge as ForgeKind;

    /// A forge serving a scripted list of pages; the cursor between them
    /// is just the next page's index. For facade tests, no network.
    struct ScriptedForge {
        pages: Vec<Vec<RepoSnapshot>>,
        fail_on: Option<usize>,
    }

    impl ScriptedForge {
        fn serving(pages: Vec<Vec<RepoSnapshot>>) -> Self {
            Self {
                pages,
                fail_on: None,
            }
        }
    }

    impl Forge for ScriptedForge {
        fn id(&self) -> &'static str {
            "scripted"
        }

        async fn starred_page(&self, cursor: Option<String>) -> Result<StarredPage, ForgeError> {
            let index = cursor.map_or(0, |text| text.parse().unwrap_or(0));
            if Some(index) == self.fail_on {
                return Err(ForgeError::Parse("scripted failure".to_string()));
            }
            let repos = self.pages.get(index).cloned().unwrap_or_default();
            let cursor = (index + 1 < self.pages.len()).then(|| (index + 1).to_string());
            Ok(StarredPage {
                repos,
                cursor,
                rate_remaining: None,
                rate_reset: None,
            })
        }

        async fn repository(&self, _repo: &RepoRef) -> Result<RepoLiveFacts, ForgeError> {
            Err(ForgeError::Absent("repository"))
        }

        async fn readme(&self, _repo: &RepoRef) -> Result<String, ForgeError> {
            Err(ForgeError::Absent("readme"))
        }

        async fn license_text(&self, _repo: &RepoRef) -> Result<Option<String>, ForgeError> {
            Ok(None)
        }

        async fn tree(
            &self,
            _repo: &RepoRef,
            _branch: &str,
        ) -> Result<(Vec<TreeEntry>, bool), ForgeError> {
            Ok((Vec::new(), false))
        }

        async fn fetch_asset(&self, _url: &str) -> Result<Vec<u8>, ForgeError> {
            Ok(Vec::new())
        }

        async fn first_commit(&self, _repo: &RepoRef) -> Result<Option<DateTime<Utc>>, ForgeError> {
            Ok(None)
        }

        async fn tarball(&self, repo: &RepoRef, _branch: &str) -> Result<DigestInput, ForgeError> {
            Ok(DigestInput {
                repo: repo.clone(),
                files: Vec::new(),
                truncated_by: None,
                taken_commit: None,
            })
        }

        async fn search(
            &self,
            _query: &DiscoverQuery,
            _now: DateTime<Utc>,
        ) -> Result<Vec<RepoSnapshot>, ForgeError> {
            Ok(Vec::new())
        }

        async fn user_starred(&self, _login: &str) -> Result<Vec<RepoSnapshot>, ForgeError> {
            Ok(Vec::new())
        }

        async fn following(&self) -> Result<Vec<String>, ForgeError> {
            Ok(Vec::new())
        }
    }

    fn snapshot(name: &str) -> RepoSnapshot {
        RepoSnapshot {
            forge: ForgeKind::GitHub,
            repo_id: 1,
            owner: "someone".to_string(),
            name: name.to_string(),
            description: None,
            stars: 0,
            languages: vec![],
            license: None,
            topics: vec![],
            default_branch: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            last_commit_at: chrono::DateTime::UNIX_EPOCH,
            starred_at: None,
            commit_count: 0,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    #[test]
    fn host_drives_a_future_on_its_runtime() {
        let host = ForgeHost::new().expect("runtime");
        let value = host.block_on(async { 40 + 2 });
        assert_eq!(value, 42);
    }

    #[test]
    fn sweep_job_streams_pages_in_order() {
        let host = ForgeHost::new().expect("runtime");
        let pages = vec![vec![snapshot("a"), snapshot("b")], vec![snapshot("c")]];
        host.block_on(async {
            let mut job = host.sweep(move || Ok(ScriptedForge::serving(pages)));
            let mut names = Vec::new();
            let mut numbers = Vec::new();
            while let Some(page) = job.next_page().await {
                let page = page.expect("page arrives");
                numbers.push(page.number);
                names.extend(page.repos.into_iter().map(|repo| repo.name));
            }
            assert_eq!(names, ["a", "b", "c"]);
            assert_eq!(numbers, [1, 2]);
        });
    }

    #[test]
    fn sweep_job_delivers_a_construction_failure_as_its_first_word() {
        let host = ForgeHost::new().expect("runtime");
        host.block_on(async {
            let mut job = host.sweep(|| -> Result<ScriptedForge, ForgeError> {
                Err(ForgeError::Parse(
                    "scripted construction failure".to_string(),
                ))
            });
            let first = job.next_page().await.expect("construction error event");
            assert!(
                first.is_err(),
                "a construction failure is the job's first word"
            );
            assert!(job.next_page().await.is_none(), "nothing after it");
        });
    }

    #[test]
    fn sweep_job_delivers_a_page_error_as_its_last_word() {
        let host = ForgeHost::new().expect("runtime");
        host.block_on(async {
            let mut job = host.sweep(move || {
                Ok(ScriptedForge {
                    pages: vec![vec![snapshot("a")], vec![]],
                    fail_on: Some(1),
                })
            });
            let first = job.next_page().await.expect("first page").expect("page ok");
            assert_eq!(first.number, 1);
            let second = job.next_page().await.expect("error event");
            assert!(second.is_err(), "the scripted failure surfaces");
            assert!(job.next_page().await.is_none(), "nothing after the error");
        });
    }

    #[test]
    fn starred_folds_every_page_into_one_list() {
        let host = ForgeHost::new().expect("runtime");
        let forge = ScriptedForge::serving(vec![
            vec![snapshot("a")],
            vec![snapshot("b"), snapshot("c")],
        ]);
        let all = host.block_on(forge.starred()).expect("full sweep");
        let names: Vec<&str> = all.iter().map(|repo| repo.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
