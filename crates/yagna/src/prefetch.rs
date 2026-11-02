//! The pre-fetch: pulling a scope's repositories into the cache before
//! the reader needs them.
//!
//! The pre-fetch is the same fetching the tab does on demand, done in
//! advance and with a plan: how many images, whether the licences, the
//! trees. The scope decides *which* repositories; the plan decides
//! *what* of each.
//!
//! The bodies (the READMEs) are always pulled — a repository without
//! its README is a repository the reader cannot read.
//!
//! Items: `run` — the pre-fetch; `Options` — the scope and the plan;
//! `Tally` — what the run did.
//! Depends on: yagna_store (the records and the scopes), yagna_forge
//! (the endpoints), crate::paths (the cache).
//! Called by: crate::main (the `prefetch` verb) and the app's tab.

use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use chrono::Utc;
use yagna_core::model::RepoRef;
use yagna_forge::{Forge, ForgeHost, GithubForge};
use yagna_store::{ImagePolicy, PrefetchPlan, PrefetchScope, QueueKind, Store};

/// What a pre-fetch covers and pulls.
pub(crate) struct Options {
    pub scope: PrefetchScope,
    pub plan: PrefetchPlan,
}

/// The verb's flags, as the command line gives them.
pub(crate) struct Flags {
    pub language: Option<String>,
    pub tag: Vec<String>,
    pub repository: Option<String>,
    pub images: String,
    pub licenses: bool,
    pub trees: bool,
}

/// The first 2 MB of a repository's images — the scoped default: enough
/// to see what a repository looks like, not the whole picture.
const FIRST_IMAGES_BYTES: u64 = 2 * 1024 * 1024;

/// Runs the verb: the flags become a scope and a plan.
pub(crate) fn run(flags: Flags) -> std::process::ExitCode {
    let scope = scope_of(&flags);
    let plan = plan_of(&flags);
    let token = match crate::token::acquire(false) {
        Some(token) => token,
        None => {
            println!("prefetch failed: no GitHub token — set GITHUB_TOKEN or run `gh auth login`");
            return std::process::ExitCode::FAILURE;
        }
    };
    let store = match crate::paths::open_catalog() {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(problem) => {
            println!("prefetch failed: {problem}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let host = match ForgeHost::new() {
        Ok(host) => host,
        Err(error) => {
            println!("prefetch failed: runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    run_with(&store, &host, &token, &Options { scope, plan })
}

/// The scope the flags name.
fn scope_of(flags: &Flags) -> PrefetchScope {
    if let Some(language) = flags.language.clone() {
        return PrefetchScope::Language(language);
    }
    if !flags.tag.is_empty() {
        return PrefetchScope::Tags(flags.tag.clone());
    }
    if let Some(repository) = flags.repository.clone() {
        let (owner, name) = repository
            .split_once('/')
            .unwrap_or(("", repository.as_str()));
        return PrefetchScope::Repository(owner.to_string(), name.to_string());
    }
    PrefetchScope::All
}

/// The plan the flags name.
fn plan_of(flags: &Flags) -> PrefetchPlan {
    let images = match flags.images.as_str() {
        "all" => ImagePolicy::All,
        "first-2mb" => ImagePolicy::FirstBytes(FIRST_IMAGES_BYTES),
        _ => ImagePolicy::None,
    };
    PrefetchPlan {
        images,
        licenses: flags.licenses,
        trees: flags.trees,
    }
}

/// Runs the pre-fetch: the scope's repositories, one after the other.
/// What is already cached is left alone, so a run that is interrupted
/// continues where it stopped.
pub(crate) fn run_with(
    store: &Arc<Mutex<Store>>,
    host: &ForgeHost,
    token: &str,
    options: &Options,
) -> ExitCode {
    if PREFETCHING.swap(true, Ordering::Relaxed) {
        println!("prefetch: one pre-fetch at a time — this one is not started");
        return ExitCode::SUCCESS;
    }
    let outcome = run_one(store, host, token, options);
    PREFETCHING.store(false, Ordering::Relaxed);
    outcome
}

/// The one pre-fetch in flight: a second start is refused while the
/// first works (the queues pace the work; parallel walkers would only
/// fight them).
static PREFETCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pre-fetch itself, already single-flight.
fn run_one(
    store: &Arc<Mutex<Store>>,
    host: &ForgeHost,
    token: &str,
    options: &Options,
) -> ExitCode {
    let started = Instant::now();
    let ids = match lock(store).scope_ids(&options.scope) {
        Ok(ids) => ids,
        Err(error) => {
            println!("prefetch failed: the scope: {error}");
            return ExitCode::FAILURE;
        }
    };
    if ids.is_empty() {
        println!("prefetch: the scope holds no repository");
        return ExitCode::SUCCESS;
    }
    let forge = match host.forge(|| GithubForge::new(token)) {
        Ok(forge) => forge,
        Err(error) => {
            println!("prefetch failed: forge: {}", error.describe());
            return ExitCode::FAILURE;
        }
    };
    crate::pacing::ensure_headroom(host, token, &mut |line| println!("{line}"));
    let mut tally = Tally::default();
    for id in ids {
        let Some(repo) = repository_of(&lock(store), id as u64) else {
            continue;
        };
        pull_one(
            store,
            host,
            &forge,
            id as u64,
            &repo,
            &options.plan,
            &mut tally,
        );
        std::thread::sleep(host.jitter_delay());
    }
    println!(
        "prefetch: {} repositories — {} readmes, {} licenses, {} trees, {} images ({} bytes), {} skipped, in {:.1}s",
        tally.repositories,
        tally.readmes,
        tally.licenses,
        tally.trees,
        tally.images,
        tally.bytes,
        tally.skipped,
        started.elapsed().as_secs_f32(),
    );
    ExitCode::SUCCESS
}

/// One repository: the bodies it lacks, and the images the plan asks
/// for.
fn pull_one(
    store: &Arc<Mutex<Store>>,
    host: &ForgeHost,
    forge: &GithubForge,
    repo_rowid: u64,
    repo: &RepoRef,
    plan: &PrefetchPlan,
    tally: &mut Tally,
) {
    tally.repositories += 1;
    let missing = match lock(store).missing_kinds(repo_rowid) {
        Ok(missing) => missing,
        Err(problem) => {
            tally.skipped += 1;
            println!("prefetch: {} could not be read — {problem}", repo.name);
            return;
        }
    };
    if missing.contains(&QueueKind::Readme) {
        match host.block_on(forge.readme(repo)) {
            Ok(text) => match lock(store).store_readme(repo_rowid, &text, Utc::now()) {
                Ok(()) => tally.readmes += 1,
                Err(problem) => {
                    tally.skipped += 1;
                    println!(
                        "prefetch: {}'s readme could not be stored — {problem}",
                        repo.name
                    );
                }
            },
            Err(problem) => tally.note_fetch_failure(&repo.name, "readme", problem),
        }
    }
    if plan.licenses && missing.contains(&QueueKind::License) {
        match host.block_on(forge.license_text(repo)) {
            Ok(Some(text)) => match lock(store).store_license_text(repo_rowid, &text, Utc::now()) {
                Ok(()) => tally.licenses += 1,
                Err(problem) => {
                    tally.skipped += 1;
                    println!(
                        "prefetch: {}'s license could not be stored — {problem}",
                        repo.name
                    );
                }
            },
            Ok(None) => {}
            Err(problem) => tally.note_fetch_failure(&repo.name, "license", problem),
        }
    }
    if plan.trees && missing.contains(&QueueKind::Tree) {
        let branch = default_branch(&lock(store), repo_rowid);
        match host.block_on(forge.tree(repo, &branch)) {
            Ok((entries, truncated)) => {
                match lock(store).store_tree(repo_rowid, &entries, truncated, Utc::now()) {
                    Ok(()) => tally.trees += 1,
                    Err(problem) => {
                        tally.skipped += 1;
                        println!(
                            "prefetch: {}'s tree could not be stored — {problem}",
                            repo.name
                        );
                    }
                }
            }
            Err(problem) => tally.note_fetch_failure(&repo.name, "tree", problem),
        }
    }
    if plan.images != ImagePolicy::None {
        pull_images(store, host, forge, repo_rowid, plan, tally);
    }
}

/// The images of one repository, up to the plan's budget.
fn pull_images(
    store: &Arc<Mutex<Store>>,
    host: &ForgeHost,
    forge: &GithubForge,
    repo_rowid: u64,
    plan: &PrefetchPlan,
    tally: &mut Tally,
) {
    let budget = match plan.images {
        ImagePolicy::None => return,
        ImagePolicy::FirstBytes(bytes) => bytes,
        ImagePolicy::All => u64::MAX,
    };
    let assets = match lock(store).repo_assets(repo_rowid) {
        Ok(assets) => assets,
        Err(_) => return,
    };
    let mut taken = lock(store).repo_asset_bytes(repo_rowid).unwrap_or(0);
    let cache = cache_dir();
    for asset in assets {
        if asset.badge || asset.state == "present" {
            continue;
        }
        if taken >= budget {
            return;
        }
        let Ok(bytes) = host.block_on(forge.fetch_asset(&asset.url)) else {
            continue;
        };
        taken += bytes.len() as u64;
        if lock(store)
            .store_asset_bytes(&asset.asset_id, "", &bytes, &cache, Utc::now())
            .is_ok()
        {
            tally.images += 1;
            tally.bytes += bytes.len() as u64;
        }
    }
}

/// Locks the shared catalog handle, recovering from a poisoned lock.
fn lock(catalog: &Arc<Mutex<Store>>) -> MutexGuard<'_, Store> {
    catalog
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The repository's identity, by its row id.
fn repository_of(store: &Store, repo_rowid: u64) -> Option<RepoRef> {
    let detail = store.repo_detail(repo_rowid).ok().flatten()?;
    Some(detail.repo_ref)
}

/// The branch the tree is read from: the repository's own, and HEAD
/// when it has none.
fn default_branch(store: &Store, repo_rowid: u64) -> String {
    store
        .repo_detail(repo_rowid)
        .ok()
        .flatten()
        .and_then(|detail| detail.default_branch)
        .unwrap_or_else(|| "HEAD".to_string())
}

/// The cache the images land in.
fn cache_dir() -> std::path::PathBuf {
    crate::paths::cache().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// What one pre-fetch did.
#[derive(Debug, Default)]
struct Tally {
    repositories: u64,
    readmes: u64,
    licenses: u64,
    trees: u64,
    images: u64,
    bytes: u64,
    /// A repository whose fetch or store did not land — said out loud,
    /// not folded into silence.
    skipped: u64,
}

impl Tally {
    /// One fetch that did not land: counted, and one line for the log.
    fn note_fetch_failure(&mut self, name: &str, what: &str, problem: yagna_forge::ForgeError) {
        self.skipped += 1;
        println!(
            "prefetch: {name}'s {what} could not be fetched — {}",
            problem.describe()
        );
    }
}
