//! The tab's own workers: the image runner, the structure fetch, the
//! exact first commit, the digest generation, and the on-demand README
//! the expanded row and the repository tab share.
//!
//! All of them run off the UI thread, on the background executor, and
//! all of them go through the facades: the store for what is stored,
//! the forge for what is fetched. None of them blocks the window, and
//! none of them renders anything — the result crosses back as data.
//!
//! Items: `REPO_BYTE_BUDGET` — the bytes one repository's images may
//! take; `run_images` — the image runner; `fetch_structure` — the
//! structure; `fetch_first_commit` — the exact first commit;
//! `fetch_digest` — the digest, through the shared pipeline;
//! `fetch_readme` — the on-demand README, one repository at a time.
//! Depends on: yagna_store (the queues and the records), yagna_forge
//! (the endpoints), crate::ui::services (the forges), crate::digestgen
//! (the digest pipeline).
//! Called by: crate::ui (the tab's actions, the row's expansion).

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use yagna_core::model::RepoRef;
use yagna_forge::{Forge, ForgeError};
use yagna_store::{FIRST_COMMIT_SOURCE_GIT_HISTORY, QueueKind, Store, StoreError};

use super::lock_catalog;
use super::services::Services;

/// The bytes one repository's images may take: past it, the rest of the
/// images wait their turn — the queue paces, not the tab.
pub const REPO_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

/// The image runner: works one repository's image queue until it is
/// empty. Four of these run at a time, and the byte budget stops the
/// rest. Returns how many images were cached.
pub(crate) async fn run_images(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    cache_dir: &Path,
    rowid: u64,
) -> u64 {
    let mut cached = 0u64;
    while work_one_image(catalog, services, cache_dir, rowid) {
        cached += 1;
    }
    cached
}

/// One image: claimed, fetched, cached. `false` when the queue is empty
/// (or the network is not there) — the runner stops.
fn work_one_image(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    cache_dir: &Path,
    rowid: u64,
) -> bool {
    let job = {
        let store = lock_catalog(catalog);
        store.claim_repo_asset(rowid).ok().flatten()
    };
    let Some(job) = job else {
        return false;
    };
    let (assets, host) = match (&services.assets, &services.host) {
        (Some(assets), Some(host)) => (assets, host),
        _ => {
            // No forges, no fetch: the claim goes back rather than
            // sitting leased for ten minutes.
            let mut store = lock_catalog(catalog);
            let _ = store.release_asset_claim(job.id, &job.claim_token, Utc::now());
            return false;
        }
    };
    let cached = {
        let store = lock_catalog(catalog);
        store.repo_asset_bytes(rowid).ok().unwrap_or(0)
    };
    let over_budget =
        |fetched: u64| cached >= REPO_BYTE_BUDGET || cached + fetched > REPO_BYTE_BUDGET;
    if over_budget(0) {
        fail_asset(catalog, &job, "the repository's image budget is spent");
        return true;
    }
    let bytes = match host.block_on(assets.fetch_asset(&job.url)) {
        Ok(bytes) => bytes,
        Err(error) => {
            handle_failure(catalog, &job, &error);
            return true;
        }
    };
    if over_budget(bytes.len() as u64) {
        fail_asset(catalog, &job, "the repository's image budget is spent");
        return true;
    }
    if !super::assets::is_rasterisable(&bytes) {
        // The rasteriser would abort the process on this one; the image
        // shows its quiet marker instead.
        fail_asset(
            catalog,
            &job,
            "the image is an SVG the renderer cannot show",
        );
        return true;
    }
    let stored = {
        let mut store = lock_catalog(catalog);
        store.store_asset_bytes(
            &job.asset_id,
            &job.claim_token,
            &bytes,
            cache_dir,
            Utc::now(),
        )
    };
    if stored.is_err() {
        fail_asset(catalog, &job, "the image could not be cached");
    }
    true
}

/// One image's failure: a 404 or an oversized image is gone for good,
/// and the asset says so — the reader shows its quiet marker. A
/// transport failure is not: the claim is released (no attempt burned)
/// and the row waits out its backoff, so an offline session shows
/// placeholders and the next pass refetches.
fn handle_failure(catalog: &Arc<Mutex<Store>>, job: &yagna_store::AssetJob, error: &ForgeError) {
    if !is_gone(error) {
        let mut store = lock_catalog(catalog);
        let _ = store.release_asset_claim(job.id, &job.claim_token, Utc::now());
        return;
    }
    fail_asset(catalog, job, &error.describe());
}

/// Whether a failed fetch means the image is not there at all.
fn is_gone(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Absent(_) | ForgeError::TooLarge { .. })
}

/// An image that cannot be fetched or cached: the queue row and the
/// asset both say so, and the reader shows its quiet marker.
fn fail_asset(catalog: &Arc<Mutex<Store>>, job: &yagna_store::AssetJob, reason: &str) {
    let mut store = lock_catalog(catalog);
    let _ = store.stamp_asset_failed(job.id, &job.claim_token, reason, Utc::now());
    let _ = store.mark_asset_failed(&job.asset_id);
}

/// The README, on demand: the expanded row's and the repository tab's
/// shared way to fetch what the store is missing — the same queue row,
/// the same writer (`store_readme`) and the same stamps the enrichment
/// runner uses, scoped to one repository. No second copy of the README
/// pipeline: the queue row is the enrichment's own.
///
/// One fetch per repository: the enqueue refuses while a live row
/// stands, so a second ask while one is in flight fetches nothing.
pub(crate) async fn fetch_readme(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    rowid: u64,
    repo: &RepoRef,
) -> Result<(), String> {
    let (api, host) = match (&services.api, &services.host) {
        (Some(api), Some(host)) => (api, host),
        _ => return Err("no token — set GITHUB_TOKEN to read".to_string()),
    };
    readme_with(catalog, host, api, rowid, repo)
}

/// The on-demand README over any forge — the worker's own path, and
/// the regression test's (a fake forge, one README, no network). The
/// body is synchronous: its one blocking move is the host's own
/// `block_on`, which must never run from inside a runtime.
pub(crate) fn readme_with<F: Forge>(
    catalog: &Arc<Mutex<Store>>,
    host: &yagna_forge::ForgeHost,
    forge: &F,
    rowid: u64,
    repo: &RepoRef,
) -> Result<(), String> {
    {
        let mut store = lock_catalog(catalog);
        store
            .enqueue_readme(rowid, Utc::now())
            .map_err(store_error)?;
    }
    let item = {
        let store = lock_catalog(catalog);
        store
            .claim_pending_for(rowid, QueueKind::Readme)
            .map_err(store_error)?
    };
    let Some(item) = item else {
        return Err("the readme is already on its way".to_string());
    };
    match host.block_on(forge.readme(repo)) {
        Ok(original) => {
            let mut store = lock_catalog(catalog);
            store
                .store_readme(rowid, &original, Utc::now())
                .map_err(store_error)?;
            store
                .stamp_done(item.id, &item.claim_token, Utc::now())
                .map_err(store_error)?;
            Ok(())
        }
        Err(ForgeError::Absent(what)) => {
            let mut store = lock_catalog(catalog);
            let reason = format!("{what} does not exist on this repository");
            let _ = store.stamp_skipped(item.id, &item.claim_token, &reason, Utc::now());
            Err(reason)
        }
        Err(error) => {
            let mut store = lock_catalog(catalog);
            stamp_fetch_failure(&mut store, &item, &error);
            Err(error.describe())
        }
    }
}

/// The structure: the tab's "load structure" action — the queue row is
/// enqueued, the tree is fetched, and the row is stored.
pub(crate) async fn fetch_structure(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    rowid: u64,
    repo: &RepoRef,
    branch: &str,
) -> Result<(), String> {
    {
        let mut store = lock_catalog(catalog);
        store.enqueue_tree(rowid, Utc::now()).map_err(store_error)?;
    }
    let item = {
        let store = lock_catalog(catalog);
        store
            .claim_pending_for(rowid, QueueKind::Tree)
            .map_err(store_error)?
    };
    let Some(item) = item else {
        return Err("the structure is already on its way".to_string());
    };
    let Some(api) = services.api.as_ref() else {
        return Err("no token — the structure cannot be fetched".to_string());
    };
    let Some(host) = services.host.as_ref() else {
        return Err("the runtime is not there — the structure cannot be fetched".to_string());
    };
    let fetched = host.block_on(api.tree(repo, branch));
    match fetched {
        Ok((entries, truncated)) => {
            let mut store = lock_catalog(catalog);
            store
                .store_tree(rowid, &entries, truncated, Utc::now())
                .map_err(store_error)?;
            store
                .stamp_done(item.id, &item.claim_token, Utc::now())
                .map_err(store_error)?;
            Ok(())
        }
        Err(error) => {
            let mut store = lock_catalog(catalog);
            stamp_fetch_failure(&mut store, &item, &error);
            Err(error.describe())
        }
    }
}

/// The license text: fetched through the forge and stored, so the tab
/// can show it.
pub(crate) async fn fetch_license(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    rowid: u64,
    repo: &RepoRef,
) -> Result<(), String> {
    {
        let mut store = lock_catalog(catalog);
        store
            .enqueue_license(rowid, Utc::now())
            .map_err(store_error)?;
    }
    let item = {
        let store = lock_catalog(catalog);
        store
            .claim_pending_for(rowid, QueueKind::License)
            .map_err(store_error)?
    };
    let Some(item) = item else {
        return Err("the license text is already on its way".to_string());
    };
    let Some(api) = services.api.as_ref() else {
        return Err("no token — the license text cannot be fetched".to_string());
    };
    let Some(host) = services.host.as_ref() else {
        return Err("the runtime is not there — the license text cannot be fetched".to_string());
    };
    let fetched = host.block_on(api.license_text(repo));
    match fetched {
        Ok(Some(body)) => {
            let mut store = lock_catalog(catalog);
            store
                .store_license_text(rowid, &body, Utc::now())
                .map_err(store_error)?;
            store
                .stamp_done(item.id, &item.claim_token, Utc::now())
                .map_err(store_error)?;
            Ok(())
        }
        Ok(None) => {
            let mut store = lock_catalog(catalog);
            let _ = store.stamp_skipped(
                item.id,
                &item.claim_token,
                "no license file exists",
                Utc::now(),
            );
            Err("this repository has no license file".to_string())
        }
        Err(error) => {
            let mut store = lock_catalog(catalog);
            stamp_fetch_failure(&mut store, &item, &error);
            Err(error.describe())
        }
    }
}

/// The exact first commit: the repository's oldest commit on its
/// default branch, and where that date came from. The source is what
/// makes it happen once.
pub(crate) async fn fetch_first_commit(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    rowid: u64,
    repo: &RepoRef,
) -> Result<bool, String> {
    let Some(api) = services.api.as_ref() else {
        return Err("no token — the first commit cannot be fetched".to_string());
    };
    let Some(host) = services.host.as_ref() else {
        return Err("the runtime is not there — the first commit cannot be fetched".to_string());
    };
    let fetched = host.block_on(api.first_commit(repo));
    let Ok(Some(date)) = fetched else {
        return Ok(false);
    };
    let mut store = lock_catalog(catalog);
    store
        .set_first_commit(rowid, date, FIRST_COMMIT_SOURCE_GIT_HISTORY)
        .map_err(store_error)?;
    Ok(true)
}

/// The digest: the tab's Generate action — the queue row is enqueued
/// (demand-only), the claimed item runs through the shared pipeline,
/// and the failure kind becomes the stamp.
pub(crate) async fn fetch_digest(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    rowid: u64,
) -> Result<(), String> {
    {
        let mut store = lock_catalog(catalog);
        store
            .enqueue_digest(rowid, Utc::now())
            .map_err(store_error)?;
    }
    let item = {
        let store = lock_catalog(catalog);
        store
            .claim_pending_for(rowid, QueueKind::Digest)
            .map_err(store_error)?
    };
    let Some(item) = item else {
        return Err("the digest is already on its way".to_string());
    };
    let Some(api) = services.api.as_ref() else {
        return Err("no token — the digest cannot be taken".to_string());
    };
    let Some(host) = services.host.as_ref() else {
        return Err("the runtime is not there — the digest cannot be taken".to_string());
    };
    let now = Utc::now();
    let outcome = {
        let mut store = lock_catalog(catalog);
        crate::digestgen::generate(&mut store, host, api, &item, now)
    };
    match outcome {
        Ok(()) => {
            let mut store = lock_catalog(catalog);
            store
                .stamp_done(item.id, &item.claim_token, Utc::now())
                .map_err(store_error)?;
            Ok(())
        }
        Err(crate::digestgen::Failure::Absent(reason)) => {
            let mut store = lock_catalog(catalog);
            let _ = store.stamp_skipped(item.id, &item.claim_token, &reason, Utc::now());
            Err(reason)
        }
        Err(crate::digestgen::Failure::RateLimited { reason, .. }) => {
            let mut store = lock_catalog(catalog);
            let _ = store.release_claim(item.id, &item.claim_token, Utc::now());
            Err(format!("{reason} — the digest will retry"))
        }
        Err(problem) => {
            let reason = problem.describe();
            let mut store = lock_catalog(catalog);
            let _ = store.stamp_attempt_failed(item.id, &item.claim_token, &reason, Utc::now());
            Err(reason)
        }
    }
}

/// One tab fetch's failure stamp: the thing legitimately not being
/// there is a skip; a rate limit releases the claim (the queue's
/// backoff paces the retry); anything else burns an attempt, so the
/// third strike fails the row.
fn stamp_fetch_failure(store: &mut Store, item: &yagna_store::QueueItem, error: &ForgeError) {
    if matches!(error, ForgeError::Absent(_)) {
        let _ = store.stamp_skipped(item.id, &item.claim_token, &error.describe(), Utc::now());
        return;
    }
    if yagna_forge::rate_pause(error).is_some() {
        let _ = store.release_claim(item.id, &item.claim_token, Utc::now());
        return;
    }
    let _ = store.stamp_attempt_failed(item.id, &item.claim_token, &error.describe(), Utc::now());
}

/// A store error, as the message the tab shows.
fn store_error(error: StoreError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn the_image_budget_is_thirty_two_megabytes() {
        assert_eq!(REPO_BYTE_BUDGET, 32 * 1024 * 1024);
    }

    /// The regression Part15 pins: a repository with no stored README,
    /// a fake forge holding one README — the on-demand worker fetches
    /// it through the enrichment queue's own row, the store writes it
    /// through the one writer (`store_readme`, so the row's preview and
    /// the tab's body — both of which read `readme_body` — hold the
    /// fetched text), and the fetch happens once per repository.
    #[test]
    fn the_on_demand_readme_fetches_once_through_the_one_writer() {
        let catalog = scratch_catalog("readme-on-demand");
        let rowid = seed_repository(&catalog);
        assert!(
            lock_catalog(&catalog).readme_body(rowid).unwrap().is_none(),
            "the repository starts with no stored README"
        );

        let forge = FakeForge::new("# Demo\n\nSome words about the thing.\n");
        let host = yagna_forge::ForgeHost::new().expect("the runtime");
        let repo = repo_of(&catalog, rowid);
        readme_with(&catalog, &host, &forge, rowid, &repo).expect("the README is fetched");

        let stored = lock_catalog(&catalog)
            .readme_body(rowid)
            .expect("read")
            .expect("the fetched text is stored");
        assert_eq!(stored.rewritten, "# Demo\n\nSome words about the thing.\n");
        assert!(!stored.truncated);
        assert_eq!(forge.fetches.load(Ordering::Relaxed), 1, "one request");

        // A second ask fetches nothing: the queue row is satisfied.
        let again = readme_with(&catalog, &host, &forge, rowid, &repo);
        assert!(again.is_err(), "the live row refuses the second ask");
        assert_eq!(
            forge.fetches.load(Ordering::Relaxed),
            1,
            "the fetch happens once per repository"
        );
    }

    /// A repository whose README does not exist is skipped with the
    /// reason recorded — never an empty README, never a burned attempt.
    #[test]
    fn a_repository_without_a_readme_is_skipped_with_its_reason() {
        let catalog = scratch_catalog("readme-absent");
        let rowid = seed_repository(&catalog);
        let forge = FakeForge::absent();
        let host = yagna_forge::ForgeHost::new().expect("the runtime");
        let repo = repo_of(&catalog, rowid);

        let outcome = readme_with(&catalog, &host, &forge, rowid, &repo);
        let problem = outcome.expect_err("the absence is said out loud");
        assert!(problem.contains("readme"), "{problem}");
        assert!(
            lock_catalog(&catalog).readme_body(rowid).unwrap().is_none(),
            "no empty README is stored"
        );
    }

    /// A scratch catalog in a temp directory, removed on drop — never
    /// in the repository.
    struct ScratchCatalog(std::path::PathBuf);

    impl ScratchCatalog {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("yagna-workers-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch directory");
            Self(dir)
        }

        fn path(&self) -> std::path::PathBuf {
            self.0.join("test.db")
        }
    }

    impl Drop for ScratchCatalog {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch_catalog(label: &str) -> Arc<Mutex<Store>> {
        let scratch = ScratchCatalog::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        // The scratch outlives the store inside the shared handle.
        std::mem::forget(scratch);
        Arc::new(Mutex::new(store))
    }

    /// Seeds one repository and returns its row id.
    fn seed_repository(catalog: &Arc<Mutex<Store>>) -> u64 {
        use chrono::TimeZone;
        use yagna_core::model::{Forge, LanguageStat, LicenseInfo};
        let at = |year: i32| {
            Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0)
                .single()
                .expect("date")
        };
        let snapshot = yagna_core::model::RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: "someone".to_string(),
            name: "something".to_string(),
            description: Some("a repo".to_string()),
            stars: 10,
            languages: vec![LanguageStat {
                name: "Rust".to_string(),
                bytes: 0,
                color: None,
            }],
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: Vec::new(),
            default_branch: Some("main".to_string()),
            created_at: at(2020),
            last_commit_at: at(2026),
            starred_at: Some(at(2026)),
            commit_count: 5,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        };
        let mut store = lock_catalog(catalog);
        store.upsert_snapshots(&[snapshot], at(2026)).expect("seed");
        store
            .repo_detail(1)
            .expect("detail")
            .expect("a row")
            .repo_rowid as u64
    }

    /// The repository's identity, as the fetch needs it.
    fn repo_of(catalog: &Arc<Mutex<Store>>, rowid: u64) -> RepoRef {
        lock_catalog(catalog)
            .repo_detail(rowid)
            .expect("detail")
            .expect("a row")
            .repo_ref
    }

    /// A forge that answers one canned README — or none at all — and
    /// counts its requests. Everything else is refused: the fake exists
    /// for the README worker's test and nothing else.
    struct FakeForge {
        readme: Option<&'static str>,
        fetches: AtomicUsize,
    }

    impl FakeForge {
        fn new(readme: &'static str) -> Self {
            Self {
                readme: Some(readme),
                fetches: AtomicUsize::new(0),
            }
        }

        fn absent() -> Self {
            Self {
                readme: None,
                fetches: AtomicUsize::new(0),
            }
        }
    }

    impl Forge for FakeForge {
        fn id(&self) -> &'static str {
            "fake"
        }

        async fn starred_page(
            &self,
            _: Option<String>,
        ) -> Result<yagna_forge::StarredPage, ForgeError> {
            Err(refused())
        }

        async fn repository(
            &self,
            _: &RepoRef,
        ) -> Result<yagna_core::model::RepoLiveFacts, ForgeError> {
            Err(ForgeError::Absent("the repository"))
        }

        async fn readme(&self, _: &RepoRef) -> Result<String, ForgeError> {
            self.fetches.fetch_add(1, Ordering::Relaxed);
            match self.readme {
                Some(text) => Ok(text.to_string()),
                None => Err(ForgeError::Absent("the readme")),
            }
        }

        async fn license_text(&self, _: &RepoRef) -> Result<Option<String>, ForgeError> {
            Err(refused())
        }

        async fn tree(
            &self,
            _: &RepoRef,
            _: &str,
        ) -> Result<(Vec<yagna_core::model::TreeEntry>, bool), ForgeError> {
            Err(refused())
        }

        async fn fetch_asset(&self, _: &str) -> Result<Vec<u8>, ForgeError> {
            Err(refused())
        }

        async fn first_commit(&self, _: &RepoRef) -> Result<Option<DateTime<Utc>>, ForgeError> {
            Err(refused())
        }

        async fn tarball(
            &self,
            _: &RepoRef,
            _: &str,
        ) -> Result<yagna_core::digest::DigestInput, ForgeError> {
            Err(refused())
        }

        async fn search(
            &self,
            _: &yagna_core::model::DiscoverQuery,
            _: DateTime<Utc>,
        ) -> Result<Vec<yagna_core::model::RepoSnapshot>, ForgeError> {
            Err(refused())
        }

        async fn user_starred(
            &self,
            _: &str,
        ) -> Result<Vec<yagna_core::model::RepoSnapshot>, ForgeError> {
            Err(refused())
        }

        async fn following(&self) -> Result<Vec<String>, ForgeError> {
            Err(refused())
        }
    }

    /// The fake's refusal for every path the test never walks.
    fn refused() -> ForgeError {
        ForgeError::Parse("the fake forge answers only readmes".to_string())
    }
}
