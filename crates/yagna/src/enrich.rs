//! The enrichment runner: works the store's enrichment queue claim by
//! claim — fetch through the forge, store through the store, stamp, and
//! move on. The asset queue follows the repository queue: the images
//! the READMEs point at are fetched anonymously and cached on disk. A
//! claimed `digest` row (demand-only, planted by a tab) generates
//! through the same pipeline the tab's own worker uses.
//!
//! The loop is serial with a jittered pause between items (secondary
//! rate limits bite rapid bursts), and it is Ctrl-C safe by
//! construction: every state change is a committed table row, so a
//! killed run loses only the item in flight. The item in flight is
//! worked under a heartbeat — a side connection renews the claim's
//! lease while the fetch runs — so a killed run's row is claimable
//! again as soon as its lease lapses. A rate limit is a visible pause,
//! never a strike: the runner waits out the named window and retries
//! the same claim, up to the stall cap, then parks the row for the
//! next run. A transient failure burns an attempt and backs off inside
//! the queue; the third strike fails the row.
//!
//! Items: `run` — the enrich phase; `work` — one claimed item's
//! fetch-and-store; `work_assets` — the asset queue's phase;
//! `Heartbeats` — the run's one side connection and thread, beating
//! for the current claim; `Row`/`through_stalls` — the shared
//! pause-park-retry loop; `Outcome` — what one item produced, with its
//! reason; `Tally`/`Totals` — the progress and summary counters.
//! Depends on: yagna_store (the queues and the storage verbs),
//! yagna_forge (the endpoints and the between-items pause), yagna_core
//! (the RepoRef the fetch needs), crate::paths (the side connection),
//! crate::pacing (the visible pauses).
//! Called by: crate::sync (after the sweep, when `--enrich` is set).

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use yagna_core::model::{Forge, RepoRef, TreeEntry};
use yagna_forge::{ForgeError, ForgeHost, GithubForge, rate_pause};
use yagna_store::{AssetJob, QueueItem, QueueKind, Store};

use crate::pacing;

/// The progress line's stride: one line every this many items per kind.
const PROGRESS_EVERY: u64 = 25;

/// How many rate-limit stalls one job may take in a run: pause and
/// retry, and on the last one park the row for the next run — the
/// quota gets its room without failing the job.
const MAX_STALLS: u32 = 3;

/// How long a parked job stays out of the way — the next run claims it
/// as usual.
const STALL_PARK_SECS: i64 = 3600;

/// The heartbeat's stride: the lease is renewed every this many
/// seconds while a job runs.
const HEARTBEAT_SECS: u64 = 60;

/// The queue's backoff horizon: when every remaining row waits longer
/// than this (a parked job), the run ends — the next run claims.
const QUEUE_WAIT_CAP: Duration = Duration::from_secs(5 * 60);

/// Runs the enrichment phase: fills the queue for every repository
/// lacking a kind, checks the rate budget, then works the queue to
/// emptiness. Never panics on a bad item — the queue records what
/// happened.
pub(crate) fn run(store: &mut Store, host: &ForgeHost, token: &str, verbose: bool) -> ExitCode {
    let started = std::time::Instant::now();
    let mut sink = |line: String| println!("{line}");
    let mut totals = Totals::default();
    for kind in [QueueKind::Readme, QueueKind::License, QueueKind::Tree] {
        match store.enqueue_missing(kind, Utc::now()) {
            Ok(queued) => {
                totals.known(kind, store.count_pending(kind).unwrap_or(0));
                if queued > 0 || verbose {
                    println!("enrich: {queued} {} item(s) queued", kind.as_str());
                }
            }
            Err(problem) => {
                println!("enrich failed: queue: {problem}");
                return ExitCode::FAILURE;
            }
        }
    }

    let forge = match host.forge(|| GithubForge::new(token)) {
        Ok(forge) => forge,
        Err(problem) => {
            println!("enrich failed: forge: {}", problem.describe());
            return ExitCode::FAILURE;
        }
    };
    pacing::ensure_headroom(host, token, &mut sink);

    let heartbeats = Heartbeats::spawn();
    let mut tally = Tally::default();
    loop {
        let item = match store.claim_pending() {
            Ok(Some(item)) => item,
            Ok(None) => {
                if !wait_out_backoff(store, &mut sink) {
                    break;
                }
                continue;
            }
            Err(problem) => {
                println!("enrich failed: queue: {problem}");
                return ExitCode::FAILURE;
            }
        };
        let row = Row::Queue {
            id: item.id,
            token: item.claim_token.clone(),
        };
        let (outcome, parked) = through_stalls(
            store,
            &row,
            &mut sink,
            |store| heartbeats.with(row.clone(), || work(store, host, &forge, &item)),
            outcome_pause,
        );
        let outcome = match parked {
            Some(detail) => Outcome::Parked(detail),
            None => outcome,
        };
        tally.record(item.kind, &outcome);
        report(&item, &outcome, &tally, &totals, verbose);
        std::thread::sleep(host.jitter_delay());
    }

    let assets = work_assets(store, host, &forge, verbose);
    let depths = store.queue_depths().unwrap_or_default();
    println!(
        "enrich: {} done, {} skipped, {} retried, {} rate-limit pauses, {} parked, {} left pending, {} failed, {} assets cached, in {:.1}s",
        tally.done_total(),
        tally.skipped,
        tally.retried,
        tally.rate_limited,
        tally.parked,
        depths.pending,
        depths.failed,
        assets,
        started.elapsed().as_secs_f32(),
    );
    let _ = store.checkpoint_wal();
    ExitCode::SUCCESS
}

/// The asset phase: the images the READMEs point at, fetched
/// anonymously and cached on disk. The queue is filled first (every
/// asset whose bytes are not cached yet), then worked to emptiness.
/// Returns how many assets were cached.
fn work_assets(store: &mut Store, host: &ForgeHost, forge: &GithubForge, verbose: bool) -> u64 {
    let now = Utc::now();
    let queued = match store.enqueue_missing_assets(now) {
        Ok(queued) => queued,
        Err(problem) => {
            println!("enrich failed: asset queue: {problem}");
            return 0;
        }
    };
    if queued > 0 || verbose {
        println!("enrich: {queued} asset(s) queued");
    }
    let mut sink = |line: String| println!("{line}");
    let heartbeats = Heartbeats::spawn();
    let mut cached = 0u64;
    loop {
        let job = match store.claim_asset() {
            Ok(Some(job)) => job,
            Ok(None) => {
                if !wait_out_asset_backoff(store, &mut sink) {
                    break;
                }
                continue;
            }
            Err(problem) => {
                println!("enrich failed: asset queue: {problem}");
                break;
            }
        };
        let row = Row::Asset {
            id: job.id,
            token: job.claim_token.clone(),
        };
        let (outcome, _parked) = through_stalls(
            store,
            &row,
            &mut sink,
            |store| heartbeats.with(row.clone(), || fetch_one_asset(store, host, forge, &job)),
            asset_pause,
        );
        match outcome {
            AssetOutcome::Done => cached += 1,
            AssetOutcome::Gone(detail) => {
                let _ = store.stamp_asset_failed(job.id, &job.claim_token, &detail, Utc::now());
                let _ = store.mark_asset_failed(&job.asset_id);
                if verbose {
                    println!("enrich: asset failed — {detail}");
                }
            }
            AssetOutcome::Failed(detail) => {
                let _ = store.release_asset_claim(job.id, &job.claim_token, Utc::now());
                if verbose {
                    println!("enrich: asset transport failed — {detail}");
                }
            }
            AssetOutcome::RateLimited(..) => {}
        }
        std::thread::sleep(host.jitter_delay());
    }
    cached
}

/// One asset: fetched anonymously, cached on disk, and stamped done —
/// or classified, for the caller to stamp.
fn fetch_one_asset(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    job: &AssetJob,
) -> AssetOutcome {
    let bytes = match host.fetch_asset(forge, &job.url) {
        Ok(bytes) => bytes,
        Err(ForgeError::Absent(what)) => {
            return AssetOutcome::Gone(format!("{what} does not exist"));
        }
        Err(error @ ForgeError::TooLarge { .. }) => return AssetOutcome::Gone(error.describe()),
        Err(error) => match rate_pause(&error) {
            Some(pause) => return AssetOutcome::RateLimited(pause, error.describe()),
            None => return AssetOutcome::Failed(error.describe()),
        },
    };
    let cache = match asset_cache() {
        Ok(cache) => cache,
        Err(problem) => return AssetOutcome::Failed(format!("cache: {problem}")),
    };
    if let Err(problem) =
        store.store_asset_bytes(&job.asset_id, &job.claim_token, &bytes, &cache, Utc::now())
    {
        return AssetOutcome::Failed(format!("cache: {problem}"));
    }
    if let Err(problem) = store.stamp_asset_done(job.id, &job.claim_token, Utc::now()) {
        return AssetOutcome::Failed(format!("catalog: {problem}"));
    }
    AssetOutcome::Done
}

/// What one asset's attempt produced.
enum AssetOutcome {
    /// Fetched, cached, stamped done.
    Done,
    /// The thing legitimately does not exist (or is over the size cap);
    /// the attempt burns.
    Gone(String),
    /// A rate limit named a wait; the row is still claimed.
    RateLimited(Duration, String),
    /// A transport or catalog failure; the row is released to wait out
    /// its backoff, no attempt burned.
    Failed(String),
}

/// The rate-limit pause an outcome names, as `through_stalls` reads it.
fn outcome_pause(outcome: &Outcome) -> Option<(Duration, String)> {
    match outcome {
        Outcome::RateLimited(pause, detail) => Some((*pause, detail.clone())),
        _ => None,
    }
}

/// The asset outcome's pause, likewise.
fn asset_pause(outcome: &AssetOutcome) -> Option<(Duration, String)> {
    match outcome {
        AssetOutcome::RateLimited(pause, detail) => Some((*pause, detail.clone())),
        _ => None,
    }
}

/// Which queue row the stall machinery is working, with the claim's
/// ownership token — the pause, park and heartbeat moves are the same
/// over both tables, and every one of them names the claim it owns.
#[derive(Clone)]
enum Row {
    Queue { id: u64, token: String },
    Asset { id: u64, token: String },
}

impl Row {
    /// Parks the row for the next run: pending again, no attempt
    /// burned, invisible to claims until the park passes. A token that
    /// no longer owns the row parks nothing.
    fn park(&self, store: &mut Store, until: DateTime<Utc>, now: DateTime<Utc>) {
        let _ = match self {
            Row::Queue { id, token } => store.stamp_rate_limited(*id, token, until, now),
            Row::Asset { id, token } => store.stamp_asset_rate_limited(*id, token, until, now),
        };
    }

    /// Renews the row's claim lease. `false`: the token no longer owns
    /// the row — the caller stops beating and lets go.
    fn heartbeat(&self, store: &Store, now: DateTime<Utc>) -> bool {
        match self {
            Row::Queue { id, token } => store.heartbeat(*id, token, now),
            Row::Asset { id, token } => store.heartbeat_asset(*id, token, now),
        }
        .unwrap_or(false)
    }
}

/// The pause-park-retry loop one rate-limited job runs through: wait
/// out the named window (visibly, the lease kept alive) and retry the
/// same claim, up to [`MAX_STALLS`]; the last stall parks the row for
/// the next run — the quota gets its room, the job is never failed.
/// The second element of the result is the park's reason, when the job
/// was parked.
fn through_stalls<O>(
    store: &mut Store,
    row: &Row,
    sink: &mut impl FnMut(String),
    mut attempt: impl FnMut(&mut Store) -> O,
    pause_of: fn(&O) -> Option<(Duration, String)>,
) -> (O, Option<String>) {
    let mut stalls = 0u32;
    loop {
        let outcome = attempt(store);
        let Some((pause, detail)) = pause_of(&outcome) else {
            return (outcome, None);
        };
        stalls += 1;
        if stalls >= MAX_STALLS || !pause_with_heartbeat(store, row, Utc::now() + pause, sink) {
            row.park(store, Utc::now() + stall_park(), Utc::now());
            sink(format!(
                "rate limit held — parking this job for the next run ({detail})"
            ));
            return (outcome, Some(detail));
        }
        sink(format!("rate limited — {detail}"));
    }
}

/// The visible pause, the claim's lease renewed across it.
fn pause_with_heartbeat(
    store: &mut Store,
    row: &Row,
    until: DateTime<Utc>,
    sink: &mut impl FnMut(String),
) -> bool {
    pacing::wait_until(
        until,
        "rate limit resets",
        &mut || {
            row.heartbeat(store, Utc::now());
        },
        sink,
        pacing::MAX_PAUSE,
    )
}

/// The queue is claimable-empty but rows wait on backoff stamps: wait
/// out the earliest (visibly) while it is near; a long park ends the
/// run — the next run claims.
fn wait_out_backoff(store: &Store, sink: &mut impl FnMut(String)) -> bool {
    let until = match store.backoff_until() {
        Ok(Some(until)) => until,
        Ok(None) => return false,
        Err(problem) => {
            sink(format!(
                "enrich failed: the queue's wait could not be read — {problem}"
            ));
            return false;
        }
    };
    pacing::wait_until(
        until,
        "the queue's backoff passes",
        &mut || {},
        sink,
        QUEUE_WAIT_CAP,
    )
}

/// The asset queue's version of the same wait.
fn wait_out_asset_backoff(store: &Store, sink: &mut impl FnMut(String)) -> bool {
    let until = match store.asset_backoff_until() {
        Ok(Some(until)) => until,
        Ok(None) => return false,
        Err(problem) => {
            sink(format!(
                "enrich failed: the asset queue's wait could not be read — {problem}"
            ));
            return false;
        }
    };
    pacing::wait_until(
        until,
        "the asset queue's backoff passes",
        &mut || {},
        sink,
        QUEUE_WAIT_CAP,
    )
}

/// The run's heartbeat: one thread and one side catalog connection for
/// the whole run — not one of each per job — beating for whichever
/// claim is in flight. A genuinely slow fetch (a digest's tarball)
/// keeps its claim; a killed run's row re-claims cleanly once the
/// lease lapses; a heartbeat the database refuses (the claim was taken
/// over, or the connection cannot write) is said out loud, never
/// swallowed — a silently dead heartbeat is how two runners end up
/// working one row.
struct Heartbeats {
    current: Arc<std::sync::Mutex<Option<Row>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeats {
    /// Spawns the run's one heartbeat thread.
    fn spawn() -> Self {
        let current = Arc::new(std::sync::Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_current = current.clone();
        let worker_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            let store = match crate::paths::open_catalog() {
                Ok(store) => store,
                Err(problem) => {
                    println!("enrich: the heartbeat side connection failed to open — {problem}");
                    return;
                }
            };
            let mut elapsed = 0u64;
            while !worker_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(1));
                elapsed += 1;
                if !elapsed.is_multiple_of(HEARTBEAT_SECS) {
                    continue;
                }
                let beat = beat_current(&worker_current, &store);
                if let Beat::Taken = beat {
                    println!(
                        "enrich: the claim on this row was taken over — another runner owns it now"
                    );
                }
            }
        });
        Self {
            current,
            stop,
            handle: Some(handle),
        }
    }

    /// Runs one job with the heartbeat beating for its claim.
    fn with<T>(&self, row: Row, job: impl FnOnce() -> T) -> T {
        *self.lock() = Some(row);
        let result = job();
        self.clear();
        result
    }

    /// Stops beating — the job is over, whatever its outcome.
    fn clear(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Row>> {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One heartbeat tick's outcome.
enum Beat {
    /// Nothing in flight, or renewed — keep going.
    Quiet,
    /// The token no longer owns the row.
    Taken,
}

/// Beats for the current claim, if there is one, and clears it when
/// the claim was taken over.
fn beat_current(current: &std::sync::Mutex<Option<Row>>, store: &Store) -> Beat {
    let mut guard = current
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(row) = guard.clone() else {
        return Beat::Quiet;
    };
    if row.heartbeat(store, Utc::now()) {
        return Beat::Quiet;
    }
    *guard = None;
    Beat::Taken
}

impl Drop for Heartbeats {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The park's length.
fn stall_park() -> chrono::Duration {
    chrono::Duration::seconds(STALL_PARK_SECS)
}

/// The asset cache's directory: where the fetched images land. An
/// unresolvable data directory is an error the caller reports — writing
/// into the process's current directory instead would be a silent
/// relocation.
fn asset_cache() -> Result<std::path::PathBuf, String> {
    crate::paths::cache().map_err(|problem| problem.to_string())
}

/// One claimed item, fetched and stored. The queue row is stamped here,
/// whatever the result.
fn work(store: &mut Store, host: &ForgeHost, forge: &GithubForge, item: &QueueItem) -> Outcome {
    let repo = RepoRef {
        forge: Forge::GitHub,
        owner: item.owner.clone(),
        name: item.name.clone(),
    };
    let now = Utc::now();
    match item.kind {
        QueueKind::Readme => readme_outcome(store, host, forge, item, &repo, now),
        QueueKind::License => license_outcome(store, host, forge, item, &repo, now),
        QueueKind::Tree => tree_outcome(store, host, forge, item, &repo, now),
        QueueKind::Digest => digest_outcome(store, host, forge, item, now),
    }
}

/// What one item's attempt produced, with the reason it carries.
enum Outcome {
    /// Fetched and stored; the row is done.
    Done,
    /// The thing legitimately does not exist; the row is skipped with
    /// the reason recorded.
    Skipped(String),
    /// A rate limit named a wait; the row is still claimed and the
    /// runner is pausing to retry it.
    RateLimited(Duration, String),
    /// The stall cap was reached (or the pause was refused); the row
    /// was parked for the next run.
    Parked(String),
    /// A transient failure; the row went back to pending (or failed on
    /// its third strike).
    AttemptFailed(String),
    /// The item could not be stored (the catalog broke); counted, and
    /// the queue keeps the error.
    Broken(String),
}

fn readme_outcome(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    item: &QueueItem,
    repo: &RepoRef,
    now: chrono::DateTime<Utc>,
) -> Outcome {
    if item.default_branch.is_none() {
        return skipped(store, item, "no default branch — nothing to fetch", now);
    }
    let fetched = host.readme(forge, repo);
    match fetched {
        Ok(original) => match store.store_readme(item.repo_rowid, &original, now) {
            Ok(()) => done(store, item, now),
            Err(problem) => broken(store, item, format!("catalog: {problem}"), now),
        },
        Err(ForgeError::Absent(what)) => skipped(
            store,
            item,
            &format!("{what} does not exist on this repository"),
            now,
        ),
        Err(error) => classify(store, item, &error, now),
    }
}

fn license_outcome(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    item: &QueueItem,
    repo: &RepoRef,
    now: chrono::DateTime<Utc>,
) -> Outcome {
    let fetched = host.license_text(forge, repo);
    match fetched {
        Ok(Some(body)) => match store.store_license_text(item.repo_rowid, &body, now) {
            Ok(()) => done(store, item, now),
            Err(problem) => broken(store, item, format!("catalog: {problem}"), now),
        },
        Ok(None) => skipped(store, item, "no license file exists", now),
        Err(error) => classify(store, item, &error, now),
    }
}

fn tree_outcome(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    item: &QueueItem,
    repo: &RepoRef,
    now: chrono::DateTime<Utc>,
) -> Outcome {
    let Some(branch) = item.default_branch.as_deref() else {
        return skipped(store, item, "no default branch — no tree exists", now);
    };
    let fetched: Result<(Vec<TreeEntry>, bool), ForgeError> = host.tree(forge, repo, branch);
    match fetched {
        Ok((entries, truncated)) => {
            match store.store_tree(item.repo_rowid, &entries, truncated, now) {
                Ok(()) => done(store, item, now),
                Err(problem) => broken(store, item, format!("catalog: {problem}"), now),
            }
        }
        Err(ForgeError::Absent(what)) => skipped(
            store,
            item,
            &format!("{what} does not exist on this repository"),
            now,
        ),
        Err(error) => classify(store, item, &error, now),
    }
}

/// The digest's generation: the pipeline does the fetch, the filter,
/// the format and the store; the failure kind becomes the stamp.
fn digest_outcome(
    store: &mut Store,
    host: &ForgeHost,
    forge: &GithubForge,
    item: &QueueItem,
    now: chrono::DateTime<Utc>,
) -> Outcome {
    match crate::digestgen::generate(store, host, forge, item, now) {
        Ok(()) => done(store, item, now),
        Err(crate::digestgen::Failure::Absent(reason)) => skipped(store, item, &reason, now),
        Err(crate::digestgen::Failure::RateLimited { reason, pause }) => {
            Outcome::RateLimited(pause, reason)
        }
        Err(crate::digestgen::Failure::Transient(reason)) => {
            attempt_failed(store, item, &reason, now)
        }
        Err(crate::digestgen::Failure::Broken(reason)) => broken(store, item, reason, now),
    }
}

/// The shared error stamp: a rate limit stays claimed for the runner's
/// pause; anything else burns an attempt.
fn classify(
    store: &mut Store,
    item: &QueueItem,
    error: &ForgeError,
    now: chrono::DateTime<Utc>,
) -> Outcome {
    if let Some(pause) = rate_pause(error) {
        return Outcome::RateLimited(pause, error.describe());
    }
    attempt_failed(store, item, &error.describe(), now)
}

/// The four stamps, one per outcome.
fn done(store: &mut Store, item: &QueueItem, now: DateTime<Utc>) -> Outcome {
    let _ = store.stamp_done(item.id, &item.claim_token, now);
    Outcome::Done
}

fn skipped(store: &mut Store, item: &QueueItem, reason: &str, now: DateTime<Utc>) -> Outcome {
    let _ = store.stamp_skipped(item.id, &item.claim_token, reason, now);
    Outcome::Skipped(reason.to_string())
}

fn attempt_failed(
    store: &mut Store,
    item: &QueueItem,
    detail: &str,
    now: DateTime<Utc>,
) -> Outcome {
    let _ = store.stamp_attempt_failed(item.id, &item.claim_token, detail, now);
    Outcome::AttemptFailed(detail.to_string())
}

fn broken(store: &mut Store, item: &QueueItem, detail: String, now: DateTime<Utc>) -> Outcome {
    let _ = store.stamp_attempt_failed(item.id, &item.claim_token, &detail, now);
    Outcome::Broken(detail)
}

/// The progress line: every `PROGRESS_EVERY` done items of one kind,
/// against the kind's starting total.
fn report(item: &QueueItem, outcome: &Outcome, tally: &Tally, totals: &Totals, verbose: bool) {
    match outcome {
        Outcome::Done => {
            let done = tally.done_of(item.kind);
            if done.is_multiple_of(PROGRESS_EVERY) {
                println!(
                    "enrich: {done}/{} {}s done",
                    totals.of(item.kind),
                    item.kind.as_str()
                );
            }
        }
        Outcome::Skipped(reason) if verbose => println!(
            "enrich: skipped {} {} — {reason}",
            item.kind.as_str(),
            item.name
        ),
        Outcome::Parked(detail) => println!(
            "enrich: {} {} parked for the next run — {detail}",
            item.kind.as_str(),
            item.name
        ),
        Outcome::AttemptFailed(detail) if verbose => println!(
            "enrich: attempt failed on {} {} — {detail}",
            item.kind.as_str(),
            item.name
        ),
        Outcome::Broken(detail) => println!(
            "enrich: {} {} could not be stored — {detail}",
            item.kind.as_str(),
            item.name
        ),
        _ => {}
    }
}

/// Per-kind counters of one run.
#[derive(Default)]
struct Tally {
    readme: u64,
    license: u64,
    tree: u64,
    digest: u64,
    skipped: u64,
    retried: u64,
    rate_limited: u64,
    parked: u64,
    broken: u64,
}

impl Tally {
    fn record(&mut self, kind: QueueKind, outcome: &Outcome) {
        match (outcome, kind) {
            (Outcome::Done, QueueKind::Readme) => self.readme += 1,
            (Outcome::Done, QueueKind::License) => self.license += 1,
            (Outcome::Done, QueueKind::Tree) => self.tree += 1,
            (Outcome::Done, QueueKind::Digest) => self.digest += 1,
            (Outcome::Skipped(_), _) => self.skipped += 1,
            (Outcome::RateLimited(..), _) => self.rate_limited += 1,
            (Outcome::Parked(_), _) => self.parked += 1,
            (Outcome::AttemptFailed(_), _) => self.retried += 1,
            (Outcome::Broken(_), _) => self.broken += 1,
        }
    }

    /// The kind's done count — the progress line's numerator.
    fn done_of(&self, kind: QueueKind) -> u64 {
        match kind {
            QueueKind::Readme => self.readme,
            QueueKind::License => self.license,
            QueueKind::Tree => self.tree,
            QueueKind::Digest => self.digest,
        }
    }

    fn done_total(&self) -> u64 {
        self.readme + self.license + self.tree + self.digest
    }
}

/// The queue's size per kind, read once after the fill.
#[derive(Default)]
struct Totals {
    readme: u64,
    license: u64,
    tree: u64,
}

impl Totals {
    fn known(&mut self, kind: QueueKind, count: u64) {
        match kind {
            QueueKind::Readme => self.readme = count,
            QueueKind::License => self.license = count,
            QueueKind::Tree => self.tree = count,
            QueueKind::Digest => {}
        }
    }

    fn of(&self, kind: QueueKind) -> u64 {
        match kind {
            QueueKind::Readme => self.readme,
            QueueKind::License => self.license,
            QueueKind::Tree => self.tree,
            QueueKind::Digest => 0,
        }
    }
}
