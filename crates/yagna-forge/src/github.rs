//! The GitHub arm of the forge seam: one GraphQL sweep over the viewer's
//! starred repositories, and the enrichment endpoints (README, license
//! text, recursive tree) the lazy layer works through.
//!
//! The sweep asks for one page at a time (cursor in, cursor out). Every
//! field the list view shows rides along — languages, license, topics,
//! the commit facts, the release, the page's remaining rate-limit
//! budget — so the sweep is the only bulk caller; everything heavier is
//! fetched lazily elsewhere. Nulls are part of the contract:
//! `primaryLanguage`, `licenseInfo`, `latestRelease` and
//! `defaultBranchRef` are all legitimately absent, and so are whole
//! edges and nodes in the connection — every one an `Option`, a hole
//! skipped, never a page discarded.
//!
//! Two shape facts the schema forces (the planning sketch predates
//! them): `languages.edges[]` carries the language under `node`, and
//! `starredAt` rides the connection's edge — `Repository` has no such
//! field. `pushedAt`, `primaryLanguage` and `owner.login` are fetched
//! per the sweep contract and deliberately unmapped: `last_commit_at`
//! (the default branch's HEAD) is the honest last commit, the store
//! reads the primary language off the languages table's first row, and
//! `nameWithOwner` is the identity.
//!
//! The enrichment endpoints reuse one fetch policy: transient failures
//! retry up the delay ladder (a server-named pause is honored), 404
//! means the thing is not there — the README falls back to its explicit
//! variant list, the license and the tree surface it as
//! `ForgeError::Absent`. Every client carries explicit connect/read
//! timeouts and octocrab's own immediate-retry ladder is off (one
//! failed call is one request; this crate's ladder owns the waiting).
//! Digest tarballs stream: wire chunks over a bounded channel into a
//! blocking walk that enforces the caps mid-stream.
//!
//! Items: `GithubForge` — the authenticated client plus the
//! credential-free asset client; `SWEEP_QUERY` — the one-page GraphQL
//! document; `REPO_QUERY` — the one-repository live-facts document;
//! the response structs — the page as GitHub sends it
//! (`SweepData::into_page` maps it; `RepoFactsData::into_live_facts`
//! maps one node); the rate-limit classifiers —
//! `rate_pause`/`is_rate_limit` (the pause-not-fail policy),
//! `LimitStamps` (their test seam); `fetch_tarball`/`walk_tarball`/
//! `pump_data`/`WireReader` — the streaming digest input;
//! `is_fetchable_url` — the asset-URL policy (http(s) only, private
//! networks refused); `tarball_url`/`encode_segment` — the codeload
//! URL; `tuned` — the one client configuration; `SEARCH_DEBOUNCE_SECS`
//! — the search tab's one-query-per-interaction floor; `RestRepo`,
//! `SearchResponse` — the REST repo objects and their mapper
//! (`to_snapshot`), shared by the search and user-starred endpoints;
//! `RestUser` — the REST user object the followed-logins endpoint
//! sends (the login is all it carries); `following_url` — the
//! followed-logins request path; `logins_of`/`unique_logins` — the
//! logins of a page, deduplicated in first-appearance order.
//! Depends on: octocrab (the client), base64 (the readme body),
//! chrono (timestamps), http_body_util (the body caps), flate2/tar
//! (the tarball walk), yagna_core::model (the snapshots and entries).
//! Called by: crate::lib (the trait's GitHub arm and the facade).

use std::collections::HashSet;
use std::future::Future;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::Deserialize;
use yagna_core::digest::{self, Caps, DigestFile, DigestInput};
use yagna_core::model::{
    DiscoverQuery, Forge as ForgeKind, LanguageStat, LicenseInfo, ReleaseInfo, RepoLiveFacts,
    RepoRef, RepoSnapshot, TreeEntry, TreeEntryKind, is_followable_login,
};

use crate::{Forge, ForgeError, StarredPage};

/// The GraphQL document for one sweep page: the viewer's starred
/// repositories, newest star first, everything the list view shows plus
/// the commit count and the latest release — and the page's remaining
/// rate-limit budget, so the runner can pause before the wall instead
/// of into it.
const SWEEP_QUERY: &str = "
query StarredSweep($cursor: String, $pageSize: Int!) {
  rateLimit { remaining resetAt }
  viewer {
    starredRepositories(first: $pageSize, after: $cursor, orderBy: {field: STARRED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      edges {
        starredAt
        node {
          databaseId
          owner { login }
          nameWithOwner
          description
          stargazerCount
          primaryLanguage { name }
          languages(first: 10, orderBy: {field: SIZE, direction: DESC}) {
            edges { size node { name color } }
          }
          licenseInfo { spdxId name }
          repositoryTopics(first: 20) { nodes { topic { name } } }
          createdAt
          pushedAt
          isFork
          isArchived
          defaultBranchRef {
            name
            target { ... on Commit { committedDate history { totalCount } } }
          }
          latestRelease { tagName publishedAt }
        }
      }
    }
  }
}";

/// How much one sweep page asks for. The API allows 100, but responses
/// past roughly 64 KB are cut off by an intermediary on some networks
/// (observed as bare 502s on every large page); 50 keeps recorded pages
/// of this shape well under that ceiling.
const SWEEP_PAGE_SIZE: i64 = 50;

/// The GraphQL document for one repository's live facts: the sweep's
/// node shape plus the open-issue count, in one core call. The node
/// reuses the sweep's mapper — one repository node, one shape.
const REPO_QUERY: &str = "
query RepoFacts($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    databaseId
    nameWithOwner
    description
    stargazerCount
    primaryLanguage { name }
    languages(first: 10, orderBy: {field: SIZE, direction: DESC}) {
      edges { size node { name color } }
    }
    licenseInfo { spdxId name }
    repositoryTopics(first: 20) { nodes { topic { name } } }
    createdAt
    isFork
    isArchived
    defaultBranchRef {
      name
      target { ... on Commit { committedDate history { totalCount } } }
    }
    latestRelease { tagName publishedAt }
    issues { totalCount }
  }
}";

/// The pauses between retries of a transient failure when GitHub names
/// no window: four attempts, then the page fails the sweep for real.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// No server-named pause is honored past this: a reset weeks away is not
/// worth hanging the sweep on — the retry ladder ends it.
const MAX_PAUSE: Duration = Duration::from_secs(300);

/// The README filename variants the raw fallback tries, in order. The
/// readme endpoint resolves branch and casing itself; this list covers
/// the 404 quirks the endpoint leaves behind.
const README_VARIANTS: [&str; 4] = ["README.md", "README.rst", "README", "readme.md"];

/// The GitHub implementation of the seam: one authenticated client, the
/// sweep page by page, the enrichment endpoints one repository at a
/// time.
pub struct GithubForge {
    client: octocrab::Octocrab,
    /// The anonymous client the asset fetches use. A README can point
    /// at any host on the internet, so the user's token must never
    /// travel with an asset request — this client carries no credential.
    assets: octocrab::Octocrab,
}

impl GithubForge {
    /// Builds the forge around a personal token. Must run where a
    /// runtime is active — the client's background middleware spawns a
    /// worker at build time — which the host's factory-based sweep and
    /// `ForgeHost::forge` both guarantee. The value travels in request
    /// headers only and is never part of an error.
    pub fn new(token: &str) -> Result<Self, ForgeError> {
        let client = tuned(octocrab::Octocrab::builder())
            .personal_token(token.to_string())
            .build()?;
        Ok(Self {
            client,
            assets: anonymous_client()?,
        })
    }

    /// Builds a forge that carries no credential at all. Only the asset
    /// endpoints are usable on it — the sweep and the enrichment
    /// endpoints need a token. The app's row preview uses it: the
    /// images it fetches live on other people's hosts.
    pub fn anonymous() -> Result<Self, ForgeError> {
        Ok(Self {
            client: anonymous_client()?,
            assets: anonymous_client()?,
        })
    }

    /// Fetches one asset's bytes, capped. The cap is enforced while the
    /// body streams, so an oversized asset is cut off rather than
    /// downloaded whole.
    async fn fetch_asset_bytes(&self, url: &str) -> Result<Vec<u8>, ForgeError> {
        if !is_fetchable_url(url) {
            return Err(ForgeError::Parse(format!(
                "the asset URL is not an http(s) URL: {url}"
            )));
        }
        let response = self.assets._get(url).await?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Err(ForgeError::Absent("the image"));
        }
        if !status.is_success() {
            return Err(ForgeError::Parse(format!(
                "the asset host answered {status}"
            )));
        }
        let limited = http_body_util::Limited::new(response.into_body(), ASSET_MAX_BYTES);
        let collected = limited
            .collect()
            .await
            .map_err(|error| body_error(error, ASSET_MAX_BYTES))?;
        Ok(collected.to_bytes().to_vec())
    }

    /// Fetches the repository's tarball from codeload and streams it
    /// through the decoder: the pump reads the compressed wire into a
    /// channel (itself capped), the blocking walk decodes and caps the
    /// content. The two run concurrently; when the walk hits a cap it
    /// drops the channel and the pump stops pulling. The response's
    /// body type is octocrab's own — it is never named here, only
    /// carried by inference into the pump.
    async fn fetch_tarball(&self, repo: &RepoRef, branch: &str) -> Result<DigestInput, ForgeError> {
        let url = tarball_url(repo, branch);
        let response = self
            .with_retries(|| async { self.client._get(&url).await.map_err(ForgeError::from) })
            .await?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Err(ForgeError::Absent("the tarball"));
        }
        if !status.is_success() {
            return Err(ForgeError::Parse(format!(
                "the tarball host answered {status}"
            )));
        }
        let repo = repo.clone();
        let (sender, receiver) = sync_channel::<Result<Vec<u8>, String>>(WIRE_CHUNK_DEPTH);
        let pump = tokio::task::spawn(async move {
            let mut body = response.into_body();
            let mut wire_bytes = 0u64;
            loop {
                let frame = match body.frame().await {
                    Some(Ok(frame)) => frame,
                    Some(Err(error)) => {
                        let _ = sender.try_send(Err(error.to_string()));
                        return;
                    }
                    None => return,
                };
                if !pump_data(&sender, frame.data_ref(), &mut wire_bytes).await {
                    return;
                }
            }
        });
        let decode = tokio::task::spawn_blocking(move || decode_tarball(receiver, repo));
        let (decoded, _) = tokio::join!(decode, pump);
        match decoded {
            Ok(result) => result,
            Err(join_error) => Err(ForgeError::Parse(format!(
                "the digest decoder failed: {join_error}"
            ))),
        }
    }

    /// Fetches one sweep page without the retry policy — the raw request.
    async fn fetch_page(&self, cursor: Option<&str>) -> Result<StarredPage, ForgeError> {
        let payload = serde_json::json!({
            "query": SWEEP_QUERY,
            "variables": { "cursor": cursor, "pageSize": SWEEP_PAGE_SIZE },
        });
        let data: SweepData = self.client.graphql(&payload).await?;
        data.into_page()
    }

    /// Fetches with the transient-failure policy: a refetch that could
    /// succeed retries up the delay ladder, pausing the server-named
    /// window when one was named. Deterministic failures pass through
    /// immediately. Every endpoint's fetch runs through here.
    async fn with_retries<T, F, Fut>(&self, fetch: F) -> Result<T, ForgeError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, ForgeError>>,
    {
        let mut attempt = 0usize;
        loop {
            match fetch().await {
                Ok(value) => return Ok(value),
                Err(error) if is_transient(&error) && attempt < RETRY_DELAYS.len() => {
                    let pause = server_pause(&error).unwrap_or(RETRY_DELAYS[attempt]);
                    tokio::time::sleep(pause).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// The one-repository live facts without the retry policy — the
    /// raw request.
    async fn fetch_facts(&self, repo: &RepoRef) -> Result<RepoLiveFacts, ForgeError> {
        let payload = serde_json::json!({
            "query": REPO_QUERY,
            "variables": { "owner": repo.owner, "name": repo.name },
        });
        let data: RepoFactsData = self.client.graphql(&payload).await?;
        data.into_live_facts()
    }

    /// The readme endpoint's answer, base64-decoded.
    async fn fetch_readme(&self, repo: &RepoRef) -> Result<String, ForgeError> {
        let content = self
            .client
            .repos(&repo.owner, &repo.name)
            .get_readme()
            .send()
            .await?;
        decode_base64_text(content.content)
    }

    /// One file's raw text over the contents endpoint, capped while the
    /// body streams — a "license file" that is really a gigabyte is
    /// cut off, not collected whole.
    async fn fetch_raw_file(&self, repo: &RepoRef, path: &str) -> Result<String, ForgeError> {
        let response = self
            .client
            .repos(&repo.owner, &repo.name)
            .get_content()
            .path(path)
            .raw()
            .send()
            .await?;
        let limited = http_body_util::Limited::new(response.into_body(), TEXT_MAX_BYTES);
        let collected = limited
            .collect()
            .await
            .map_err(|error| body_error(error, TEXT_MAX_BYTES))?;
        Ok(String::from_utf8_lossy(&collected.to_bytes()).into_owned())
    }

    /// The `LICENSE*` first match in the repository root listing: its
    /// full path, or none when no license file sits at the root. A 404
    /// listing (an empty repository) is no license, not a failure.
    async fn fetch_license_path(&self, repo: &RepoRef) -> Result<Option<String>, ForgeError> {
        let mut listing = match self
            .with_retries(|| async {
                Ok(self
                    .client
                    .repos(&repo.owner, &repo.name)
                    .get_content()
                    .send()
                    .await?)
            })
            .await
        {
            Ok(listing) => listing,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(license_path_of(listing.take_items()))
    }

    /// The recursive tree at `branch`, mapped to core entries, the
    /// truncation flag carried.
    async fn fetch_tree(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<(Vec<TreeEntry>, bool), ForgeError> {
        let git_tree = self
            .client
            .repos(&repo.owner, &repo.name)
            .get_tree(branch)
            .recursive(true)
            .send()
            .await?;
        let entries = git_tree.tree.iter().filter_map(tree_entry_of).collect();
        Ok((entries, git_tree.truncated))
    }

    /// The repository's oldest commit on its default branch.
    ///
    /// The commits endpoint's page carries the last page's link, so the
    /// oldest commit is two requests away however deep the history is:
    /// the pages are never walked. A repository with no commits answers
    /// with an empty page — there is no first commit to report.
    async fn fetch_first_commit(
        &self,
        repo: &RepoRef,
    ) -> Result<Option<DateTime<Utc>>, ForgeError> {
        let first = self
            .client
            .repos(&repo.owner, &repo.name)
            .list_commits()
            .per_page(1)
            .send()
            .await?;
        let Some(last) = first.last else {
            return Ok(first.items.last().and_then(commit_date));
        };
        let page: octocrab::Page<octocrab::models::repos::RepoCommit> =
            self.client.get(last.to_string(), None::<&()>).await?;
        Ok(page.items.last().and_then(commit_date))
    }

    /// One page of the authenticated user's followed logins: the user
    /// records as the endpoint sends them. The request carries the
    /// token — the list belongs to the token's own user.
    async fn fetch_following_page(&self, page: u32) -> Result<Vec<RestUser>, ForgeError> {
        let url = following_url(page);
        let body: serde_json::Value = self
            .with_retries(|| async { Ok(self.client.get(&url, None::<&()>).await?) })
            .await
            .map_err(following_page_error)?;
        following_users_of(body)
    }
}

/// The instant a commit was written: the author's date, and the
/// committer's when the author carries none.
fn commit_date(commit: &octocrab::models::repos::RepoCommit) -> Option<DateTime<Utc>> {
    commit
        .commit
        .author
        .as_ref()
        .and_then(|author| author.date)
        .or_else(|| {
            commit
                .commit
                .committer
                .as_ref()
                .and_then(|committer| committer.date)
        })
}

/// The root listing's `LICENSE*` first match (case-insensitive, files
/// only): its full path.
fn license_path_of(items: Vec<octocrab::models::repos::Content>) -> Option<String> {
    items
        .iter()
        .find(|item| item.r#type == "file" && item.name.to_lowercase().starts_with("license"))
        .map(|item| item.path.clone())
}

/// One git tree entry as a core entry; unknown kinds drop.
fn tree_entry_of(entry: &octocrab::models::git::GitTreeEntry) -> Option<TreeEntry> {
    let kind = match entry.r#type.as_str() {
        "blob" => TreeEntryKind::Blob,
        "tree" => TreeEntryKind::Tree,
        "commit" => TreeEntryKind::Commit,
        _ => return None,
    };
    Some(TreeEntry {
        path: entry.path.clone(),
        kind,
        size: entry.size.map(|size| size as u64),
    })
}

/// Decodes the contents API's base64 body. Whitespace inside the
/// payload is squeezed out (the API line-wraps it); a missing body
/// means the file sits over the API's inline size cap.
fn decode_base64_text(payload: Option<String>) -> Result<String, ForgeError> {
    let Some(payload) = payload else {
        return Err(ForgeError::Parse(
            "the file body is missing (over the API's inline size cap)".to_string(),
        ));
    };
    let squeezed: String = payload
        .chars()
        .filter(|char| !char.is_whitespace())
        .collect();
    let bytes = base64::prelude::BASE64_STANDARD
        .decode(squeezed.as_bytes())
        .map_err(|error| ForgeError::Parse(format!("the base64 body failed to decode: {error}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

impl Forge for GithubForge {
    fn id(&self) -> &'static str {
        ForgeKind::GitHub.as_str()
    }

    /// One sweep page, pausing and retrying when refetching could
    /// succeed: rate limits (pause, not fail — a retry after the named
    /// window is expected to succeed) and transport hiccups (empty or
    /// truncated bodies, 5xx answers, connection errors). Deterministic
    /// failures — a malformed response, a query the schema rejects, a
    /// 401 — are not retried; they would repeat identically.
    async fn starred_page(&self, cursor: Option<String>) -> Result<StarredPage, ForgeError> {
        self.with_retries(|| async { self.fetch_page(cursor.as_deref()).await })
            .await
    }

    /// One repository's live facts in a single GraphQL call — the
    /// sweep's node shape plus the open issues — under the transient
    /// retry policy. A repository that is not there is `Absent`.
    async fn repository(&self, repo: &RepoRef) -> Result<RepoLiveFacts, ForgeError> {
        self.with_retries(|| async { self.fetch_facts(repo).await })
            .await
    }

    /// The README: the endpoint first (it resolves branch and filename
    /// case), the explicit variant list over the contents endpoint when
    /// it answers 404, and `Absent` when nothing answers — the
    /// repository has no README at all.
    async fn readme(&self, repo: &RepoRef) -> Result<String, ForgeError> {
        match self
            .with_retries(|| async { self.fetch_readme(repo).await })
            .await
        {
            Ok(text) => return Ok(text),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        for variant in README_VARIANTS {
            match self
                .with_retries(|| async { self.fetch_raw_file(repo, variant).await })
                .await
            {
                Ok(text) => return Ok(text),
                Err(error) if is_not_found(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ForgeError::Absent("readme"))
    }

    /// The license file's text: the root listing's `LICENSE*` first
    /// match, fetched raw. `Ok(None)` when no license file exists.
    async fn license_text(&self, repo: &RepoRef) -> Result<Option<String>, ForgeError> {
        let Some(path) = self.fetch_license_path(repo).await? else {
            return Ok(None);
        };
        let path_text = self
            .with_retries(|| async { self.fetch_raw_file(repo, &path).await })
            .await?;
        Ok(Some(path_text))
    }

    /// The recursive tree at `branch`; a 404 (an empty repository, a
    /// branch that vanished) is `Absent`, not a transport failure.
    async fn tree(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<(Vec<TreeEntry>, bool), ForgeError> {
        match self
            .with_retries(|| async { self.fetch_tree(repo, branch).await })
            .await
        {
            Ok(tree) => Ok(tree),
            Err(error) if is_not_found(&error) => Err(ForgeError::Absent("tree")),
            Err(error) => Err(error),
        }
    }

    /// One asset's bytes — the image a README points at. The request is
    /// anonymous and the body is capped.
    async fn fetch_asset(&self, url: &str) -> Result<Vec<u8>, ForgeError> {
        self.fetch_asset_bytes(url).await
    }

    /// The repository's exact first commit: the oldest commit on its
    /// default branch, or none when it has no commits.
    async fn first_commit(&self, repo: &RepoRef) -> Result<Option<DateTime<Utc>>, ForgeError> {
        self.with_retries(|| async { self.fetch_first_commit(repo).await })
            .await
    }

    /// The tarball at `branch`, streamed and decoded into the digest
    /// input, caps enforced mid-stream. A 404 (a branch or a repository
    /// that is not there) is `Absent`.
    async fn tarball(&self, repo: &RepoRef, branch: &str) -> Result<DigestInput, ForgeError> {
        self.fetch_tarball(repo, branch).await
    }

    /// The DISCOVER query against the search API. One query per
    /// interaction — the caller debounces at `SEARCH_DEBOUNCE_SECS` —
    /// and the 30 req/min budget is the probe's `search_remaining`
    /// beside the controls.
    async fn search(
        &self,
        query: &DiscoverQuery,
        now: DateTime<Utc>,
    ) -> Result<Vec<RepoSnapshot>, ForgeError> {
        let url = search_url(query, now);
        let response: SearchResponse = self
            .with_retries(|| async { Ok(self.client.get(&url, None::<&()>).await?) })
            .await?;
        Ok(snapshots_of(response.items))
    }

    /// Another user's public stars, newest page first, capped at
    /// `MAX_USER_STARRED_PAGES` of one hundred — shadows are views, not
    /// history, and the store caps them again at `MAX_SHADOWS_PER_LOGIN`.
    async fn user_starred(&self, login: &str) -> Result<Vec<RepoSnapshot>, ForgeError> {
        if !is_followable_login(login) {
            return Err(ForgeError::Parse(format!(
                "the login is not a GitHub username: {login}"
            )));
        }
        let mut all = Vec::new();
        for page in 1..=MAX_USER_STARRED_PAGES {
            let url = user_starred_url(login, page);
            let items: Vec<RestRepo> = self
                .with_retries(|| async { Ok(self.client.get(&url, None::<&()>).await?) })
                .await?;
            let done = items.len() < USER_STARRED_PER_PAGE as usize;
            all.extend(snapshots_of(items));
            if done {
                break;
            }
        }
        Ok(all)
    }

    /// The logins the authenticated user follows: the people the
    /// token's own account follows, walked over at most
    /// `MAX_FOLLOWING_PAGES` pages and stopped early when a page comes
    /// back short. The duplicates collapse — the first appearance keeps
    /// its place — and an empty list is a valid answer.
    async fn following(&self) -> Result<Vec<String>, ForgeError> {
        let mut all = Vec::new();
        for page in 1..=MAX_FOLLOWING_PAGES {
            let users = self.fetch_following_page(page).await?;
            let short = users.len() < FOLLOWING_PER_PAGE as usize;
            all.extend(logins_of(users));
            if short {
                break;
            }
        }
        Ok(unique_logins(all))
    }
}

/// How many repositories one DISCOVER search asks for: the OhNiceRepo
/// ceiling — one page, no "more" beyond a repeat query.
pub const SEARCH_PER_PAGE: u8 = 30;

/// How many pages of another user's stars one refresh walks: four
/// pages of one hundred, then the store's own cap decides.
pub const MAX_USER_STARRED_PAGES: u32 = 4;

/// One page of another user's stars.
const USER_STARRED_PER_PAGE: u8 = 100;

/// How many pages of followed logins one call walks: three pages of
/// one hundred. The list is a suggestion source, not a census — three
/// hundred names is more than any surface shows.
const MAX_FOLLOWING_PAGES: u32 = 3;

/// One page of followed logins.
const FOLLOWING_PER_PAGE: u8 = 100;

/// The search API's request path for one DISCOVER query: the `q`
/// value percent-encoded, the API's sort alongside it. The response
/// shape is the repo-object list, so the sweep's mapper is reused —
/// one code path maps both.
fn search_url(query: &DiscoverQuery, now: DateTime<Utc>) -> String {
    format!(
        "/search/repositories?q={}&sort={}&order=desc&per_page={}",
        encode_query_value(&query.query_text(now)),
        query.sort.param(),
        SEARCH_PER_PAGE,
    )
}

/// One page of another user's public stars.
fn user_starred_url(login: &str, page: u32) -> String {
    format!(
        "/users/{}/starred?per_page={}&page={page}",
        encode_segment(login),
        USER_STARRED_PER_PAGE,
    )
}

/// One page of the authenticated user's followed logins. The path is
/// the token's own user — no login rides along.
fn following_url(page: u32) -> String {
    format!(
        "/user/following?per_page={}&page={page}",
        FOLLOWING_PER_PAGE
    )
}

/// One query value percent-encoded: unreserved characters pass
/// through, everything else — spaces, colons, angle brackets — rides
/// as `%XX`. The search query legitimately carries all three.
fn encode_query_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let plain = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if plain {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// One REST repository object, as the search and starred endpoints
/// send it. Field names are the API's snake_case; every legitimately
/// absent field is an `Option`, and a record that is not `owner/name`
/// is skipped by the mapper rather than failing the page.
#[derive(Debug, Deserialize)]
struct RestRepo {
    id: Option<i64>,
    full_name: String,
    description: Option<String>,
    stargazers_count: Option<i64>,
    language: Option<String>,
    license: Option<RestLicense>,
    topics: Option<Vec<String>>,
    created_at: String,
    pushed_at: Option<String>,
    default_branch: Option<String>,
    fork: Option<bool>,
    archived: Option<bool>,
}

/// The license object inside a REST repository.
#[derive(Debug, Deserialize)]
struct RestLicense {
    spdx_id: Option<String>,
    name: Option<String>,
    key: Option<String>,
}

/// The search API's response: the repo objects under `items`.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<RestRepo>,
}

/// One REST user object, as the followed-logins endpoint sends it. The
/// login is the only field the suggestion source needs; a record
/// without one contributes nothing.
#[derive(Debug, Deserialize)]
struct RestUser {
    login: Option<String>,
}

/// Every REST record into a snapshot; a record that is not
/// `owner/name` or carries no parseable timestamp is skipped — one bad
/// record must not discard the page it rode in on.
fn snapshots_of(items: Vec<RestRepo>) -> Vec<RepoSnapshot> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.to_snapshot(index).ok())
        .collect()
}

/// The logins of one page, in the service's order. A record without a
/// usable login contributes nothing.
fn logins_of(users: Vec<RestUser>) -> Vec<String> {
    users
        .into_iter()
        .filter_map(|user| user.login)
        .map(|login| login.trim().to_string())
        .filter(|login| !login.is_empty())
        .collect()
}

/// The logins with the duplicates collapsed: the first appearance
/// keeps its place, the later ones go.
fn unique_logins(logins: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for login in logins {
        if seen.insert(login.clone()) {
            unique.push(login);
        }
    }
    unique
}

/// The page's user objects: the array of user records the endpoint
/// sends. A body that is not that array is a parse error — never an
/// empty list, which would read as "nobody is followed".
fn following_users_of(body: serde_json::Value) -> Result<Vec<RestUser>, ForgeError> {
    serde_json::from_value(body)
        .map_err(|error| ForgeError::Parse(format!("the following page failed to parse: {error}")))
}

/// A page whose body would not parse is a parse error, not a transport
/// failure: the body was malformed, and the logins must not come back
/// silently empty. Every other failure keeps its kind — a rate limit
/// stays a rate limit.
fn following_page_error(error: ForgeError) -> ForgeError {
    let ForgeError::Github(octocrab::Error::Json { source, .. }) = error else {
        return error;
    };
    ForgeError::Parse(format!("the following page failed to parse: {source}"))
}

impl RestRepo {
    /// One REST repository into a snapshot. `index` names the record
    /// in mapping errors, so a bad record is findable in a page.
    fn to_snapshot(&self, index: usize) -> Result<RepoSnapshot, ForgeError> {
        let at = format!("rest {index}");
        let (owner, name) = split_identity(&self.full_name, &at)?;
        let created_at = parse_rfc3339(&self.created_at, &at, "created_at")?;
        let last_commit_at = match self.pushed_at.as_deref() {
            Some(pushed) => parse_rfc3339(pushed, &at, "pushed_at")?,
            None => created_at,
        };
        let languages = match self.language.as_deref() {
            Some(language) => vec![LanguageStat {
                name: language.to_string(),
                bytes: 0,
                color: None,
            }],
            None => Vec::new(),
        };
        let license = self.license.as_ref().and_then(|license| {
            let name = license.name.clone().or_else(|| license.key.clone())?;
            Some(LicenseInfo {
                name,
                spdx_id: license.spdx_id.clone(),
            })
        });
        Ok(RepoSnapshot {
            forge: ForgeKind::GitHub,
            repo_id: self.id.map(|id| id.max(0) as u64).unwrap_or(0),
            owner,
            name,
            description: self.description.clone(),
            stars: self
                .stargazers_count
                .map(|count| count.max(0) as u64)
                .unwrap_or(0),
            languages,
            license,
            topics: self.topics.clone().unwrap_or_default(),
            default_branch: self.default_branch.clone(),
            created_at,
            last_commit_at,
            starred_at: None,
            commit_count: 0,
            latest_release: None,
            is_fork: self.fork.unwrap_or(false),
            is_archived: self.archived.unwrap_or(false),
        })
    }
}

/// The asset byte cap: past this the asset is refused. A README's
/// screenshot is a few hundred kilobytes; five megabytes is a monster
/// already.
pub const ASSET_MAX_BYTES: usize = 5 * 1024 * 1024;

/// The text-endpoint byte cap (raw license/README variants): past this
/// the fetch is refused. Generous over the largest real license text,
/// small next to what a hostile repository could serve.
const TEXT_MAX_BYTES: usize = 1024 * 1024;

/// How long a connection may take to establish, and how long a read
/// may stall, before the call fails as a transport error. The HTTP
/// client's own defaults are infinite — a stalled connection would
/// hang a verb forever with no message, so the forge names its
/// patience instead.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The builder as `Octocrab::builder()` hands it over — named once so
/// the one configuration below can take and return it.
type FreshBuilder = octocrab::OctocrabBuilder<
    octocrab::NoSvc,
    octocrab::DefaultOctocrabBuilderConfig,
    octocrab::NoAuth,
    octocrab::NotLayerReady,
>;

/// One builder configured the way every client in the forge is: the
/// timeouts set, and octocrab's built-in retry ladder off — it retries
/// immediately, which is exactly the rapid burst the secondary rate
/// limit punishes, and it would stack under this crate's own ladder
/// (which backs off and pauses for the named window). One failed call
/// is one request; the ladder above decides the rest.
pub(crate) fn tuned(builder: FreshBuilder) -> FreshBuilder {
    use octocrab::service::middleware::retry::RetryConfig;
    builder
        .set_connect_timeout(Some(CONNECT_TIMEOUT))
        .set_read_timeout(Some(READ_TIMEOUT))
        .add_retry_config(RetryConfig::None)
}

/// A client that carries no credential — the one the asset fetches use.
fn anonymous_client() -> Result<octocrab::Octocrab, ForgeError> {
    Ok(tuned(octocrab::Octocrab::builder()).build()?)
}

/// Whether a URL may be fetched: http and https only, and not into a
/// private network. A README can carry anything — the fetcher must not
/// be a file reader, and it must not be talked into probing the
/// reader's own machines (a README's author is not the reader's
/// friend, and the cloud-metadata address is one hop from every
/// credential in the house).
fn is_fetchable_url(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    !is_private_host(host)
}

/// Whether a host names a private or loopback address: the ranges the
/// reader's own machines live in, plus the link-local block the cloud
/// metadata services answer on. IPv6 literals arrive bracketed.
fn is_private_host(host: &str) -> bool {
    if host.starts_with('[') {
        let inner = host
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or_default();
        return matches!(inner, "::1" | "::")
            || inner.starts_with("fe80:")
            || inner.starts_with("fc")
            || inner.starts_with("fd");
    }
    if host == "localhost" {
        return true;
    }
    let mut parts = host.split(':').next().unwrap_or_default().split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("127"), _, _, _)
        | (Some("10"), _, _, _)
        | (Some("0"), _, _, _)
        | (Some("192"), Some("168"), _, _)
        | (Some("169"), Some("254"), _, _) => true,
        (Some("172"), Some(second), _, _) => {
            matches!(second.parse::<u8>(), Ok(n) if (16..=31).contains(&n))
        }
        _ => false,
    }
}

/// The body error of a capped fetch: a length-limit failure names the
/// cap that was hit (the body is too big), anything else is the
/// transport's.
fn body_error(error: Box<dyn std::error::Error + Send + Sync>, limit: usize) -> ForgeError {
    if error
        .downcast_ref::<http_body_util::LengthLimitError>()
        .is_some()
    {
        return ForgeError::TooLarge { limit };
    }
    ForgeError::Asset(error.to_string())
}

// --- The digest tarball: codeload's gzip'd tar, streamed into the
// --- formatter's input. The pump moves wire bytes into a channel on
// --- the runtime; the walk decodes on the blocking pool, so neither
// --- the compressed nor the decompressed body is ever held whole.

/// How many wire chunks may sit in the channel between the pump and
/// the decoder: a small backlog keeps the HTTP connection moving
/// without holding the tarball in memory.
const WIRE_CHUNK_DEPTH: usize = 8;

/// The caps the walk enforces. The public path uses the digest
/// module's defaults; the tests walk with tiny ones.
#[derive(Debug, Clone, Copy)]
struct WalkCaps {
    total_bytes: u64,
    max_files: usize,
    max_file_bytes: u64,
}

impl WalkCaps {
    /// The digest module's own defaults.
    fn defaults() -> Self {
        Self {
            total_bytes: digest::DEFAULT_TOTAL_BYTES,
            max_files: digest::DEFAULT_MAX_FILES,
            max_file_bytes: digest::DEFAULT_MAX_FILE_BYTES,
        }
    }
}

/// The codeload URL of one branch's tarball, each path segment
/// percent-encoded where the URL grammar demands it — a branch name
/// may carry a space or a `#`, and those must not be read as the URL's
/// own syntax.
fn tarball_url(repo: &RepoRef, branch: &str) -> String {
    format!(
        "https://codeload.github.com/{}/{}/tar.gz/refs/heads/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(branch),
    )
}

/// One URL path segment: unreserved characters (and `/`, which branch
/// names legitimately contain) pass through, everything else is
/// percent-encoded.
fn encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let plain =
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/');
        if plain {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// One frame's bytes into the channel: a control frame carries none
/// and keeps the pump going; data goes over the channel until the wire
/// cap stops it. `false` ends the pump — the decoder walked away, or
/// the tarball is too big for the wire. The send never parks a runtime
/// thread: a full channel waits a beat for the blocking decoder to
/// drain it instead.
async fn pump_data<T>(
    sender: &SyncSender<Result<Vec<u8>, String>>,
    data: Option<&T>,
    wire_bytes: &mut u64,
) -> bool
where
    T: std::ops::Deref<Target = [u8]>,
{
    let Some(data) = data else {
        return true;
    };
    *wire_bytes += data.len() as u64;
    if !send_when_room(sender, Ok(data.to_vec())).await {
        return false;
    }
    if *wire_bytes > digest::DEFAULT_TOTAL_BYTES {
        let _ = sender.try_send(Err(format!(
            "the tarball's compressed body passed the {} byte wire cap",
            digest::DEFAULT_TOTAL_BYTES
        )));
        return false;
    }
    true
}

/// One chunk over the channel, yielding while the decoder is behind —
/// a blocking `send` here would park a runtime worker thread.
async fn send_when_room(
    sender: &SyncSender<Result<Vec<u8>, String>>,
    chunk: Result<Vec<u8>, String>,
) -> bool {
    let mut chunk = chunk;
    loop {
        match sender.try_send(chunk) {
            Ok(()) => return true,
            Err(std::sync::mpsc::TrySendError::Full(back)) => {
                chunk = back;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

/// The decoder on the blocking pool: the wire chunks as a `Read`, the
/// gzip on top, the tar walk over that.
fn decode_tarball(
    receiver: Receiver<Result<Vec<u8>, String>>,
    repo: RepoRef,
) -> Result<DigestInput, ForgeError> {
    let wire = WireReader {
        receiver,
        buffer: Vec::new(),
        position: 0,
    };
    walk_tarball(wire, repo, WalkCaps::defaults())
}

/// The tar walk itself: entries into `DigestFile`s, the root commit
/// read off the tarball's root directory name, the caps enforced as the
/// walk goes. A file over the per-file cap is listed without its body
/// (its bytes are never read); a walk past the file-count or
/// total-bytes cap stops, and the input says which cap stopped it.
fn walk_tarball<R: Read>(
    wire: R,
    repo: RepoRef,
    caps: WalkCaps,
) -> Result<DigestInput, ForgeError> {
    let gz = flate2::read::MultiGzDecoder::new(wire);
    let mut archive = tar::Archive::new(gz);
    let mut taken_commit: Option<String> = None;
    let mut files: Vec<DigestFile> = Vec::new();
    let mut content_bytes = 0u64;
    let mut truncated: Option<Caps> = None;
    let entries = archive.entries().map_err(tarball_error)?;
    for entry in entries {
        if truncated.is_some() {
            break;
        }
        let mut entry = entry.map_err(tarball_error)?;
        let path = entry.path().map_err(tarball_error)?.to_path_buf();
        if taken_commit.is_none() {
            taken_commit = root_commit_of(&path, &repo);
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(path) = strip_root(&path) else {
            continue;
        };
        let size = entry.header().size().map_err(tarball_error)?;
        if files.len() + 1 > caps.max_files {
            truncated = Some(Caps::FileCount {
                limit: caps.max_files,
            });
            break;
        }
        if size > caps.max_file_bytes {
            files.push(DigestFile {
                path,
                bytes: size,
                text: None,
            });
            continue;
        }
        if content_bytes + size > caps.total_bytes {
            truncated = Some(Caps::TotalBytes {
                limit: caps.total_bytes,
            });
            break;
        }
        let mut content = Vec::with_capacity(size as usize);
        entry.read_to_end(&mut content).map_err(tarball_error)?;
        content_bytes += size;
        files.push(DigestFile {
            path,
            bytes: size,
            text: digest::text_of(&content),
        });
    }
    Ok(DigestInput {
        repo,
        files,
        truncated_by: truncated,
        taken_commit,
    })
}

/// The tarball root's commit sha: GitHub names the root directory
/// `owner-repo-<sha>`, and the full identity prefix is known, so what
/// remains after it is the sha. A hex run of at least seven characters
/// is accepted — the tarball's own naming is the source, and a root
/// that does not fit the shape contributes no commit.
fn root_commit_of(path: &Path, repo: &RepoRef) -> Option<String> {
    let first = path.components().next()?.as_os_str().to_str()?;
    let prefix = format!("{}-{}-", repo.owner, repo.name);
    let sha = first.strip_prefix(&prefix)?;
    let looks_like_sha =
        sha.len() >= 7 && sha.chars().all(|character| character.is_ascii_hexdigit());
    looks_like_sha.then(|| sha.to_string())
}

/// The path inside the repository: the root directory's component
/// removed. A path with nothing under the root (the root directory
/// itself, the pax global header) contributes no file.
fn strip_root(path: &Path) -> Option<String> {
    let mut components = path.components();
    components.next()?;
    let rest = components.as_path().to_str()?;
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

/// The wire as a `Read`: chunks out of the channel, a transport error
/// becomes an io error, a closed channel is the end of the wire.
struct WireReader {
    receiver: Receiver<Result<Vec<u8>, String>>,
    buffer: Vec<u8>,
    position: usize,
}

impl Read for WireReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.position >= self.buffer.len() {
            match self.receiver.recv() {
                Ok(Ok(chunk)) => {
                    self.buffer = chunk;
                    self.position = 0;
                }
                Ok(Err(problem)) => return Err(std::io::Error::other(problem)),
                Err(_) => return Ok(0),
            }
        }
        let take = out.len().min(self.buffer.len() - self.position);
        out[..take].copy_from_slice(&self.buffer[self.position..self.position + take]);
        self.position += take;
        Ok(take)
    }
}

/// A decode failure is the tarball's own fault — the asset-flavoured
/// error carries what went wrong.
fn tarball_error(error: std::io::Error) -> ForgeError {
    ForgeError::Asset(format!("the tarball could not be read: {error}"))
}

/// True when refetching the page could succeed: rate limits and
/// transport hiccups. An allowlist, not a denylist — an error kind the
/// policy does not know fails immediately rather than looping.
fn is_transient(error: &ForgeError) -> bool {
    let ForgeError::Github(error) = error else {
        return false;
    };
    match error {
        octocrab::Error::GitHub { source, .. } => {
            let status = source.status_code.as_u16();
            status >= 500
                || status == 429
                || (status == 403
                    && (source.rate_limit_reset.is_some() || retry_after_value(source).is_some()))
        }
        octocrab::Error::Json { .. }
        | octocrab::Error::Service { .. }
        | octocrab::Error::Http { .. }
        | octocrab::Error::Hyper { .. } => true,
        _ => false,
    }
}

/// True when GitHub answered 404: the thing asked for is legitimately
/// not there — a fallback case, never a failure of the transport.
fn is_not_found(error: &ForgeError) -> bool {
    let ForgeError::Github(error) = error else {
        return false;
    };
    let octocrab::Error::GitHub { source, .. } = error else {
        return false;
    };
    source.status_code.as_u16() == 404
}

/// The wait the error names for a rate limit, when it names one: the
/// primary limit's reset window, or a secondary limit's `Retry-After`.
/// Uncapped — the queue's pause is visible and may run an hour; the
/// in-forge retry ladder keeps its own shorter cap. The reset window
/// wins when a 403 carries both stamps.
pub fn rate_pause(error: &ForgeError) -> Option<Duration> {
    let ForgeError::Github(octocrab::Error::GitHub { source, .. }) = error else {
        return None;
    };
    rate_pause_stamps(
        LimitStamps {
            status: source.status_code.as_u16(),
            reset_at: source.rate_limit_reset_at(),
            retry_after: retry_after_value(source),
        },
        Utc::now(),
    )
}

/// The wait a secondary limit asks for when it names nothing — GitHub's
/// own guidance for an unexplained 429 is one minute of patience.
const SECONDARY_PAUSE: Duration = Duration::from_secs(60);

/// The stamps a rate-limit response carries, as the classifiers read
/// them. Real `octocrab::GitHubError`s are `#[non_exhaustive]` — no
/// test can build one — so injection happens one step out, here.
pub(crate) struct LimitStamps<'a> {
    pub(crate) status: u16,
    pub(crate) reset_at: Option<DateTime<Utc>>,
    pub(crate) retry_after: Option<&'a str>,
}

/// The classification itself: which wait the stamps name, if any. A
/// 429 is always a rate limit — with no named window, the secondary
/// default is the wait; a 403 must name a window to be one.
fn rate_pause_stamps(stamps: LimitStamps<'_>, now: DateTime<Utc>) -> Option<Duration> {
    match stamps.status {
        429 => wait_until(stamps.reset_at, now, None)
            .or_else(|| retry_after_wait(stamps.retry_after, now, None))
            .or(Some(SECONDARY_PAUSE)),
        403 => wait_until(stamps.reset_at, now, None)
            .or_else(|| retry_after_wait(stamps.retry_after, now, None)),
        _ => None,
    }
}

/// The wait until a reset instant: one second of grace past the stamp,
/// one second of grace when the stamp already passed (clock skew, not
/// an excuse to misread the limit as a generic failure), and `None`
/// when the wait would outrun a cap.
fn wait_until(
    reset_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    cap: Option<Duration>,
) -> Option<Duration> {
    let reset_at = reset_at?;
    let wait = (reset_at - now).to_std().unwrap_or(Duration::ZERO);
    within_cap(wait + Duration::from_secs(1), cap)
}

/// The `Retry-After` header's wait: delta-seconds, or an HTTP-date —
/// both are legal answers, and the pause is honored whole either way.
fn retry_after_wait(
    value: Option<&str>,
    now: DateTime<Utc>,
    cap: Option<Duration>,
) -> Option<Duration> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return within_cap(Duration::from_secs(seconds), cap);
    }
    let date = DateTime::parse_from_rfc2822(value).ok()?;
    let wait = (date.with_timezone(&Utc) - now).to_std().ok()?;
    within_cap(wait, cap)
}

/// The pause GitHub named, when it named one: the rate-limit reset time
/// or the `Retry-After` header, whichever it sent, capped to the
/// ladder's patience.
fn server_pause(error: &ForgeError) -> Option<Duration> {
    let ForgeError::Github(octocrab::Error::GitHub { source, .. }) = error else {
        return None;
    };
    let now = Utc::now();
    wait_until(source.rate_limit_reset_at(), now, Some(MAX_PAUSE))
        .or_else(|| retry_after_wait(retry_after_value(source), now, Some(MAX_PAUSE)))
}

/// A wait inside the cap — `None` for a cap of `None` means uncapped,
/// not refused.
fn within_cap(wait: Duration, cap: Option<Duration>) -> Option<Duration> {
    match cap {
        Some(cap) if wait > cap => None,
        _ => Some(wait),
    }
}

/// The `Retry-After` header's raw value, when present.
fn retry_after_value(source: &octocrab::GitHubError) -> Option<&str> {
    source.headers.as_ref()?.get("retry-after")?.to_str().ok()
}

// --- The sweep response, as GitHub sends it. Every nullable field is an
// --- Option: the null-heavy fixture is the pinned proof.

/// The `data` object the client unwraps to. The fields are camelCase
/// because the query is — one missed rename here is one silently dead
/// field (serde skips unknown names and `Option` reads as `None`), so
/// the fixture carries every ride-along the query asks for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SweepData {
    rate_limit: Option<RateLimitWire>,
    viewer: Viewer,
}

/// The sweep's free ride-along: the page's remaining core budget and
/// the instant it resets. Both legitimately null on an error-shaped
/// response — the runner treats an absent budget as "no news".
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWire {
    remaining: Option<i64>,
    reset_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewer {
    starred_repositories: StarredConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StarredConnection {
    page_info: PageInfo,
    /// The schema allows a null edge in the list; a null is skipped,
    /// not page-fatal.
    edges: Vec<Option<StarredEdge>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct StarredEdge {
    #[serde(rename = "starredAt")]
    starred_at: Option<String>,
    /// A null node is a hole in the list, not a page-fatal record: it
    /// is skipped like a null edge.
    node: Option<RepoNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoNode {
    database_id: Option<i64>,
    name_with_owner: String,
    description: Option<String>,
    stargazer_count: Option<i64>,
    languages: Option<LanguageConnection>,
    license_info: Option<LicenseWire>,
    repository_topics: Option<TopicsConnection>,
    created_at: String,
    is_fork: bool,
    is_archived: bool,
    default_branch_ref: Option<BranchRef>,
    latest_release: Option<ReleaseWire>,
    /// The open-issue count — a ride-along only the one-repository
    /// query asks for; the sweep query never requests it and reads
    /// `None`.
    issues: Option<IssuesWire>,
}

#[derive(Deserialize)]
struct IssuesWire {
    #[serde(rename = "totalCount")]
    total_count: Option<i64>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct LanguageConnection {
    edges: Option<Vec<Option<LanguageEdge>>>,
}

#[derive(Deserialize)]
struct LanguageEdge {
    size: Option<i64>,
    node: Option<LanguageNode>,
}

#[derive(Deserialize)]
struct LanguageNode {
    name: String,
    color: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LicenseWire {
    spdx_id: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct TopicsConnection {
    nodes: Option<Vec<Option<TopicNode>>>,
}

#[derive(Deserialize)]
struct TopicNode {
    topic: Named,
}

#[derive(Deserialize)]
struct BranchRef {
    name: String,
    target: Option<CommitTarget>,
}

/// The inline-fragment projection of the default branch's head commit.
/// A branch pointing at something that is not a commit deserializes to
/// an empty struct: no commit facts.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitTarget {
    committed_date: Option<String>,
    history: Option<History>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct History {
    total_count: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseWire {
    tag_name: String,
    published_at: Option<String>,
}

impl SweepData {
    /// The page: every edge mapped into a snapshot (null edges and null
    /// nodes are holes, skipped — one bad record must not discard the
    /// other forty-nine), the cursor kept only when GitHub names one.
    fn into_page(self) -> Result<StarredPage, ForgeError> {
        let connection = self.viewer.starred_repositories;
        let repos = connection
            .edges
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, edge)| edge.to_snapshot(index))
            .collect::<Result<Vec<Option<_>>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let cursor = page_cursor(&connection.page_info)?;
        let (rate_remaining, rate_reset) = match self.rate_limit {
            Some(limit) => (
                limit.remaining.map(|value| value.max(0) as u64),
                // A malformed reset stamp degrades to "no news" — it is
                // a diagnostic field, never worth the page it rode in on.
                limit
                    .reset_at
                    .as_deref()
                    .and_then(|text| parse_rfc3339(text, "page", "rateLimit.resetAt").ok()),
            ),
            None => (None, None),
        };
        Ok(StarredPage {
            repos,
            cursor,
            rate_remaining,
            rate_reset,
        })
    }
}

/// The `data` object of the one-repository query: the node itself,
/// null when the repository is not there.
#[derive(Deserialize)]
struct RepoFactsData {
    repository: Option<RepoNode>,
}

impl RepoFactsData {
    /// The live facts: the sweep's node mapper plus the open issues. A
    /// null repository is `Absent`, not an empty fact set — "nothing
    /// known" and "gone" are different answers.
    fn into_live_facts(self) -> Result<RepoLiveFacts, ForgeError> {
        let node = self.repository.ok_or(ForgeError::Absent("repository"))?;
        let open_issues = node
            .issues
            .as_ref()
            .and_then(|issues| issues.total_count)
            .unwrap_or(0)
            .max(0) as u64;
        Ok(RepoLiveFacts {
            snapshot: node.to_snapshot(0, None)?,
            open_issues,
        })
    }
}

/// The page's cursor: `None` at the end, `Some` mid-sweep, and an
/// error when the page claims more but names no cursor — reading that
/// as "done" would end the sweep early and call it success.
fn page_cursor(info: &PageInfo) -> Result<Option<String>, ForgeError> {
    match (info.has_next_page, info.end_cursor.as_ref()) {
        (true, Some(cursor)) => Ok(Some(cursor.clone())),
        (true, None) => Err(ForgeError::Parse(
            "the page says a next page exists but names no cursor".to_string(),
        )),
        (false, _) => Ok(None),
    }
}

impl StarredEdge {
    /// One starred edge into a snapshot: `starredAt` rides the edge, the
    /// rest on the repository node inside it.
    fn to_snapshot(&self, index: usize) -> Result<Option<RepoSnapshot>, ForgeError> {
        let Some(node) = self.node.as_ref() else {
            return Ok(None);
        };
        node.to_snapshot(index, self.starred_at.as_deref())
            .map(Some)
    }
}

impl RepoNode {
    /// One repository node into a snapshot. `index` names the node in
    /// parse errors, so a bad record is findable in a page of fifty.
    fn to_snapshot(
        &self,
        index: usize,
        starred_at: Option<&str>,
    ) -> Result<RepoSnapshot, ForgeError> {
        let at = format!("node {index}");
        let (owner, name) = split_identity(&self.name_with_owner, &at)?;
        let created_at = parse_rfc3339(&self.created_at, &at, "createdAt")?;
        let starred_at = match starred_at {
            Some(text) => Some(parse_rfc3339(text, &at, "starredAt")?),
            None => None,
        };
        let license = self.license_info.as_ref().map(|license| LicenseInfo {
            name: license.name.clone(),
            spdx_id: license.spdx_id.clone(),
        });
        let latest_release = match self.latest_release.as_ref() {
            Some(release) => Some(ReleaseInfo {
                tag: release.tag_name.clone(),
                published_at: release
                    .published_at
                    .as_deref()
                    .map(|text| parse_rfc3339(text, &at, "latestRelease.publishedAt"))
                    .transpose()?,
            }),
            None => None,
        };
        let default_branch = self
            .default_branch_ref
            .as_ref()
            .map(|branch| branch.name.clone());
        let (commit_count, last_commit_at) =
            commit_facts(self.default_branch_ref.as_ref(), created_at);
        Ok(RepoSnapshot {
            forge: ForgeKind::GitHub,
            repo_id: self.database_id.map(|id| id.max(0) as u64).unwrap_or(0),
            owner,
            name,
            description: self.description.clone(),
            stars: self
                .stargazer_count
                .map(|count| count.max(0) as u64)
                .unwrap_or(0),
            languages: languages_of(self.languages.as_ref()),
            license,
            topics: topics_of(self.repository_topics.as_ref()),
            default_branch,
            created_at,
            last_commit_at,
            starred_at,
            commit_count,
            latest_release,
            is_fork: self.is_fork,
            is_archived: self.is_archived,
        })
    }
}

/// Splits `owner/name`. The identity is the one field that cannot be
/// defaulted: without it the row has no key.
fn split_identity(combined: &str, at: &str) -> Result<(String, String), ForgeError> {
    let (owner, name) = combined
        .split_once('/')
        .ok_or_else(|| ForgeError::Parse(format!("{at}: {combined:?} is not owner/name")))?;
    Ok((owner.to_string(), name.to_string()))
}

fn parse_rfc3339(text: &str, at: &str, field: &str) -> Result<DateTime<Utc>, ForgeError> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| ForgeError::Parse(format!("{at}: {field} is not a timestamp: {error}")))
}

/// The commit count and the last-commit instant from the default branch.
/// An empty repository has no default branch at all: zero commits, and
/// the creation instant stands in as the last one (the snapshot contract
/// says so — `commit_count` tells the two cases apart).
fn commit_facts(branch: Option<&BranchRef>, created_at: DateTime<Utc>) -> (u64, DateTime<Utc>) {
    let Some(branch) = branch else {
        return (0, created_at);
    };
    let count = branch
        .target
        .as_ref()
        .and_then(|target| target.history.as_ref())
        .and_then(|history| history.total_count)
        .map(|count| count.max(0) as u64)
        .unwrap_or(0);
    let committed = branch
        .target
        .as_ref()
        .and_then(|target| target.committed_date.as_deref())
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&Utc));
    (count, committed.unwrap_or(created_at))
}

/// The language table, biggest first; edges without a language inside
/// are skipped.
fn languages_of(connection: Option<&LanguageConnection>) -> Vec<LanguageStat> {
    let Some(edges) = connection.and_then(|connection| connection.edges.as_ref()) else {
        return Vec::new();
    };
    edges
        .iter()
        .flatten()
        .filter_map(|edge| {
            let node = edge.node.as_ref()?;
            Some(LanguageStat {
                name: node.name.clone(),
                bytes: edge.size.map(|size| size.max(0) as u64).unwrap_or(0),
                color: node.color.clone(),
            })
        })
        .collect()
}

/// The topic names, in the service's order.
fn topics_of(connection: Option<&TopicsConnection>) -> Vec<String> {
    let Some(nodes) = connection.and_then(|connection| connection.nodes.as_ref()) else {
        return Vec::new();
    };
    nodes
        .iter()
        .flatten()
        .map(|node| node.topic.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads a recorded response fixture from the crate's fixture
    /// directory: recorded bodies, parsed offline — no network.
    fn fixture_text(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(path).expect("fixture readable")
    }

    /// A sweep-page fixture, parsed into the response structs.
    fn sweep_fixture(name: &str) -> SweepData {
        serde_json::from_str(&fixture_text(name)).expect("fixture is a sweep response")
    }

    #[test]
    fn parses_the_recorded_small_page() {
        let page = sweep_fixture("small-page.json").into_page().expect("page");
        assert_eq!(page.repos.len(), 3);
        assert!(page.cursor.is_some(), "the recorded page has a next page");

        let first = &page.repos[0];
        assert_eq!(first.owner, "Nuu-maan");
        assert_eq!(first.name, "ttysvg");
        assert_eq!(first.repo_id, 1_320_194_494);
        assert_eq!(first.stars, 9);
        assert_eq!(
            first.description.as_deref(),
            Some(
                "Record your terminal, get one self-contained animated SVG for your README. Works on Windows."
            )
        );
        assert_eq!(first.default_branch.as_deref(), Some("main"));
        assert_eq!(first.languages.len(), 1);
        assert_eq!(first.languages[0].name, "Rust");
        assert_eq!(first.languages[0].bytes, 199_402);
        assert_eq!(first.languages[0].color.as_deref(), Some("#dea584"));
        let license = first.license.as_ref().expect("licensed");
        assert_eq!(license.name, "MIT License");
        assert_eq!(license.spdx_id.as_deref(), Some("MIT"));
        assert_eq!(first.topics.len(), 7);
        assert!(first.topics.contains(&"terminal".to_string()));
        assert_eq!(first.commit_count, 82);
        assert!(first.starred_at.is_some());
        assert!(!first.is_fork);
        assert!(!first.is_archived);

        // The release carries its publish instant.
        let third = &page.repos[2];
        let release = third.latest_release.as_ref().expect("released");
        assert_eq!(release.tag, "v0.11.0");
        assert!(release.published_at.is_some());
    }

    #[test]
    fn parses_every_null_in_the_recorded_page() {
        let page = sweep_fixture("null-heavy-page.json")
            .into_page()
            .expect("page");
        assert_eq!(page.repos.len(), 4);

        // No primary language, no language edges, no release — but
        // licensed and topical.
        let crawler = &page.repos[0];
        assert!(crawler.languages.is_empty());
        assert!(crawler.latest_release.is_none());
        assert!(crawler.license.is_some());
        assert_eq!(crawler.topics.len(), 7);

        // No license, no topics, no languages, no release.
        let bare = &page.repos[1];
        assert_eq!(bare.license, None);
        assert!(bare.topics.is_empty());
        assert!(bare.languages.is_empty());
        assert!(bare.latest_release.is_none());

        // No description, but a full commit history.
        let undescribed = &page.repos[2];
        assert_eq!(undescribed.description, None);
        assert!(undescribed.commit_count > 0);

        // The empty repository: no default branch, no push instant —
        // zero commits, and creation stands in as the last commit.
        let empty = &page.repos[3];
        assert_eq!(empty.commit_count, 0);
        assert_eq!(empty.last_commit_at, empty.created_at);
        assert_eq!(empty.default_branch, None);
        assert_eq!(empty.description, None);
        assert_eq!(empty.license, None);
        assert!(empty.topics.is_empty());
        assert!(empty.languages.is_empty());
        assert!(empty.latest_release.is_none());
        assert!(empty.starred_at.is_some());
    }

    #[test]
    fn the_page_carries_its_rate_limit_budget() {
        // The ride-along the query asks for: a `rateLimit` block at the
        // response's top level, camelCase like the query. One missed
        // rename here and the pacing never fires — so the fixture is
        // built the way the wire sends it.
        let mut value: serde_json::Value =
            serde_json::from_str(&fixture_text("small-page.json")).expect("fixture is JSON");
        value["rateLimit"] = serde_json::json!({
            "remaining": 4998,
            "resetAt": "2026-10-30T12:00:00Z",
        });
        let data: SweepData =
            serde_json::from_value(value).expect("a sweep response with a budget");
        let page = data.into_page().expect("page");
        assert_eq!(page.rate_remaining, Some(4998));
        assert_eq!(
            page.rate_reset.map(|reset| reset.to_rfc3339()),
            Some("2026-10-30T12:00:00+00:00".to_string())
        );
    }

    #[test]
    fn a_malformed_reset_stamp_degrades_but_keeps_the_page() {
        let mut value: serde_json::Value =
            serde_json::from_str(&fixture_text("small-page.json")).expect("fixture is JSON");
        value["rateLimit"] = serde_json::json!({ "remaining": 10, "resetAt": "not a date" });
        let data: SweepData = serde_json::from_value(value).expect("a sweep response");
        let page = data.into_page().expect("the page survives the stamp");
        assert_eq!(page.rate_remaining, Some(10));
        assert_eq!(page.rate_reset, None, "the stamp degrades to no news");
    }

    #[test]
    fn null_edges_and_nodes_are_holes_not_page_failures() {
        let mut value: serde_json::Value =
            serde_json::from_str(&fixture_text("small-page.json")).expect("fixture is JSON");
        value["viewer"]["starredRepositories"]["edges"][1] = serde_json::Value::Null;
        value["viewer"]["starredRepositories"]["edges"][2]["node"] = serde_json::Value::Null;
        let data: SweepData = serde_json::from_value(value).expect("still a sweep response");
        let page = data.into_page().expect("the holes are skipped, not fatal");
        assert_eq!(
            page.repos.len(),
            1,
            "one live record survives: {:?}",
            page.repos
        );
    }

    #[test]
    fn a_page_claiming_more_without_a_cursor_is_an_error() {
        let mut value: serde_json::Value =
            serde_json::from_str(&fixture_text("small-page.json")).expect("fixture is JSON");
        value["viewer"]["starredRepositories"]["pageInfo"]["endCursor"] = serde_json::Value::Null;
        let data: SweepData = serde_json::from_value(value).expect("still a sweep response");
        assert!(
            data.into_page().is_err(),
            "a next page without a cursor must not read as done"
        );
    }

    #[test]
    fn the_fetchable_url_policy_refuses_private_networks() {
        for url in [
            "https://raw.githubusercontent.com/a/b/c.png",
            "http://cdn.example.org/x.gif",
        ] {
            assert!(is_fetchable_url(url), "public: {url}");
        }
        for url in [
            "https://localhost/x.png",
            "https://127.0.0.1/x.png",
            "http://10.1.2.3/x.png",
            "http://192.168.1.1/router.png",
            "http://172.16.0.9/inner.png",
            "http://172.31.255.255/inner.png",
            "http://169.254.169.254/latest/meta-data",
            "https://[::1]/x.png",
            "https://[fe80::1]/x.png",
            "ftp://example.org/x.png",
            "file:///etc/passwd",
        ] {
            assert!(!is_fetchable_url(url), "refused: {url}");
        }
        assert!(
            is_fetchable_url("https://172.32.0.1/x.png"),
            "172.32 is public"
        );
    }

    #[test]
    fn the_tarball_url_encodes_its_branch_segment() {
        let repo = RepoRef {
            forge: ForgeKind::GitHub,
            owner: "someone".to_string(),
            name: "something".to_string(),
        };
        assert_eq!(
            tarball_url(&repo, "main"),
            "https://codeload.github.com/someone/something/tar.gz/refs/heads/main"
        );
        assert_eq!(
            tarball_url(&repo, "feature/next thing"),
            "https://codeload.github.com/someone/something/tar.gz/refs/heads/feature/next%20thing"
        );
    }

    #[test]
    fn a_node_without_an_identity_fails_loudly() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/small-page.json"),
        )
        .expect("fixture readable");
        let mut value: serde_json::Value = serde_json::from_str(&text).expect("fixture is JSON");
        value["viewer"]["starredRepositories"]["edges"][0]["node"]["nameWithOwner"] =
            serde_json::json!("no-slash");
        let data: SweepData = serde_json::from_value(value).expect("still a sweep response");
        let error = data.into_page().expect_err("identity rejected");
        let ForgeError::Parse(text) = error else {
            panic!("expected a parse error, got {error:?}");
        };
        assert!(
            text.contains("no-slash"),
            "the error names the value: {text}"
        );
    }

    #[test]
    fn the_query_asks_for_a_page_of_variables() {
        assert!(SWEEP_QUERY.contains("$cursor: String"));
        assert!(SWEEP_QUERY.contains("$pageSize: Int!"));
        assert!(SWEEP_QUERY.contains("STARRED_AT"));
    }

    #[test]
    fn the_repo_facts_fixture_maps_the_same_node_mapper_plus_issues() {
        let response: RepoFactsData =
            serde_json::from_str(&fixture_text("repo-facts.json")).expect("fixture is repo facts");
        let facts = response.into_live_facts().expect("facts");
        assert_eq!(facts.snapshot.owner, "Nuu-maan");
        assert_eq!(facts.snapshot.name, "ttysvg");
        assert_eq!(facts.snapshot.stars, 9);
        assert_eq!(facts.snapshot.commit_count, 82);
        assert_eq!(facts.snapshot.default_branch.as_deref(), Some("main"));
        assert!(
            facts.snapshot.starred_at.is_none(),
            "a live fetch is not a star event"
        );
        let release = facts.snapshot.latest_release.as_ref().expect("released");
        assert_eq!(release.tag, "v0.11.0");
        assert_eq!(facts.open_issues, 12);
    }

    #[test]
    fn a_repository_that_is_not_there_is_absent() {
        let response: RepoFactsData =
            serde_json::from_str(r#"{"repository":null}"#).expect("null repository");
        let error = response.into_live_facts().expect_err("absent");
        assert!(matches!(error, ForgeError::Absent("repository")));
    }

    #[test]
    fn the_repo_query_asks_for_the_live_facts_in_one_call() {
        assert!(REPO_QUERY.contains("$owner: String!"));
        assert!(REPO_QUERY.contains("$name: String!"));
        assert!(REPO_QUERY.contains("latestRelease"));
        assert!(REPO_QUERY.contains("issues"));
        assert!(REPO_QUERY.contains("history { totalCount }"));
    }

    #[test]
    fn the_issues_field_is_ride_along_and_absent_on_the_sweep() {
        let node: RepoNode = serde_json::from_str(
            r#"{"nameWithOwner":"a/b","createdAt":"2026-01-15T10:00:00Z","isFork":false,"isArchived":false}"#,
        )
        .expect("node");
        assert!(node.issues.is_none(), "the sweep query never asks for it");
    }

    #[test]
    fn the_search_fixture_maps_the_same_mapper_as_the_sweep() {
        let response: SearchResponse = serde_json::from_str(&fixture_text("search-repos.json"))
            .expect("fixture is search JSON");
        let snapshots = snapshots_of(response.items);
        assert_eq!(snapshots.len(), 2);
        let first = &snapshots[0];
        assert_eq!(first.owner, "someone");
        assert_eq!(first.name, "fast-search");
        assert_eq!(first.stars, 1_200);
        assert_eq!(first.languages.len(), 1);
        assert_eq!(first.languages[0].name, "Rust");
        let license = first.license.as_ref().expect("licensed");
        assert_eq!(license.spdx_id.as_deref(), Some("MIT"));
        assert_eq!(first.topics, ["search", "rust"]);
        assert!(first.starred_at.is_none());
        let bare = &snapshots[1];
        assert!(bare.languages.is_empty());
        assert_eq!(bare.license, None);
        assert_eq!(bare.last_commit_at, bare.created_at);
    }

    #[test]
    fn the_user_starred_fixture_maps_nulls_to_defaults() {
        let items: Vec<RestRepo> = serde_json::from_str(&fixture_text("user-starred.json"))
            .expect("fixture is starred JSON");
        let snapshots = snapshots_of(items);
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].owner, "friend");
        assert_eq!(snapshots[0].stars, 42);
        assert!(snapshots[1].is_fork);
        assert_eq!(snapshots[1].default_branch, None);
    }

    /// The recorded followed-logins page, embedded at compile time: the
    /// fixture never travels with the binary and is never read from
    /// disk at run time.
    const FOLLOWING_FIXTURE: &str = include_str!("../tests/fixtures/following-users.json");

    #[test]
    fn the_following_url_carries_the_page_size_and_the_page_number() {
        assert_eq!(following_url(1), "/user/following?per_page=100&page=1");
        assert_eq!(following_url(3), "/user/following?per_page=100&page=3");
    }

    #[test]
    fn the_following_fixture_maps_to_its_logins_in_order() {
        let body: serde_json::Value =
            serde_json::from_str(FOLLOWING_FIXTURE).expect("fixture is a following page");
        let users = following_users_of(body).expect("the page is the user array");
        let logins = unique_logins(logins_of(users));
        assert_eq!(
            logins,
            ["octocat", "hubot", "monalisa"],
            "the service's order, the repeat collapsed"
        );
    }

    #[test]
    fn a_following_record_without_a_usable_login_contributes_nothing() {
        let users: Vec<RestUser> = serde_json::from_str(
            r#"[{"login": ""}, {"login": "octocat"}, {"login": null}, {"id": 9}]"#,
        )
        .expect("a following page");
        assert_eq!(logins_of(users), ["octocat"]);
    }

    #[test]
    fn a_page_that_is_not_the_user_array_is_a_parse_error_not_an_empty_list() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"message": "Not Found"}"#).expect("fixture is JSON");
        let error = following_users_of(body).expect_err("an object is not the user array");
        assert!(
            matches!(error, ForgeError::Parse(_)),
            "the page fails as a parse error: {error:?}"
        );
    }

    #[test]
    fn a_page_error_that_is_not_a_parse_failure_keeps_its_kind() {
        let error = following_page_error(ForgeError::Absent("the following page"));
        assert!(
            matches!(error, ForgeError::Absent(_)),
            "a rate limit stays a rate limit: {error:?}"
        );
    }

    #[test]
    fn a_rest_record_without_an_identity_is_skipped_not_page_fatal() {
        let bad = RestRepo {
            id: None,
            full_name: "no-slash".to_string(),
            description: None,
            stargazers_count: None,
            language: None,
            license: None,
            topics: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            pushed_at: None,
            default_branch: None,
            fork: None,
            archived: None,
        };
        assert!(bad.to_snapshot(0).is_err());
        assert!(snapshots_of(vec![bad]).is_empty());
    }

    #[test]
    fn the_discover_url_carries_the_query_shape_and_the_sort() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 10, 31, 12, 0, 0)
            .single()
            .expect("valid test date");
        let query = DiscoverQuery {
            created_within_days: 30,
            min_stars: 200,
            language: Some("rust".to_string()),
            topic: None,
            word: None,
            sort: yagna_core::model::DiscoverSort::Stars,
        };
        let url = search_url(&query, now);
        assert!(url.starts_with("/search/repositories?q="));
        assert!(url.contains("sort=stars"));
        assert!(url.contains("per_page=30"));
        assert!(
            url.contains("created"),
            "the created-after floor rides along: {url}"
        );
        let people = user_starred_url("octocat", 2);
        assert_eq!(people, "/users/octocat/starred?per_page=100&page=2");
    }

    #[test]
    fn the_discover_url_carries_the_topic_and_the_quoted_word() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 10, 31, 12, 0, 0)
            .single()
            .expect("valid test date");
        let query = DiscoverQuery {
            topic: Some("wasm".to_string()),
            word: Some("web server".to_string()),
            ..DiscoverQuery::default()
        };
        let url = search_url(&query, now);
        assert!(
            url.contains("topic%3Awasm"),
            "the topic rides as its qualifier: {url}"
        );
        assert!(
            url.contains("%22web%20server%22"),
            "the word rides quoted, so it stays a word: {url}"
        );
    }

    #[test]
    fn unusable_logins_never_reach_the_network() {
        assert!(!is_followable_login(""));
        assert!(!is_followable_login("-lead"));
    }

    #[test]
    fn pause_helpers_honor_the_server_window_and_cap_it() {
        let now = Utc::now();
        let soon = now + chrono::Duration::seconds(10);
        assert_eq!(
            wait_until(Some(soon), now, Some(MAX_PAUSE)),
            Some(Duration::from_secs(11)),
            "one second of grace over the reset time"
        );
        let far = now + chrono::Duration::hours(2);
        assert_eq!(
            wait_until(Some(far), now, Some(MAX_PAUSE)),
            None,
            "a reset hours away is not worth hanging on"
        );
        assert_eq!(wait_until(None, now, Some(MAX_PAUSE)), None);
        assert_eq!(
            wait_until(Some(now - chrono::Duration::seconds(5)), now, None),
            Some(Duration::from_secs(1)),
            "a stamp in the past is one second of grace, not a refusal"
        );
        assert_eq!(
            retry_after_wait(Some("7"), now, None),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after_wait(Some("99999"), now, Some(MAX_PAUSE)),
            None,
            "a Retry-After beyond the ladder's patience is refused, not clamped"
        );
        let whole = DateTime::from_timestamp(now.timestamp(), 0).expect("a whole-second instant");
        let http_date =
            DateTime::from_timestamp(now.timestamp() + 90, 0).expect("a whole-second instant");
        assert_eq!(
            retry_after_wait(Some(&http_date.to_rfc2822()), whole, None),
            Some(Duration::from_secs(90)),
            "an HTTP-date Retry-After is a wait too"
        );
        assert_eq!(retry_after_wait(Some("soon"), now, None), None);
        assert_eq!(retry_after_wait(None, now, None), None);
    }

    #[test]
    fn the_readme_fixture_decodes_to_its_text() {
        let content: octocrab::models::repos::Content =
            serde_json::from_str(&fixture_text("readme-content.json"))
                .expect("fixture is a contents response");
        assert_eq!(content.encoding.as_deref(), Some("base64"));
        let text = decode_base64_text(content.content).expect("decoded");
        assert_eq!(text, "# Demo\n\nA tool that does one thing well.\n");
    }

    #[test]
    fn a_missing_or_broken_base64_body_is_a_parse_error_not_a_panic() {
        let error = decode_base64_text(None).expect_err("missing body");
        assert!(matches!(error, ForgeError::Parse(_)));
        let error = decode_base64_text(Some("!!!not base64!!!".to_string())).expect_err("garbage");
        assert!(matches!(error, ForgeError::Parse(text) if text.contains("base64")));

        // The API line-wraps payloads; whitespace squeezes out first.
        let text =
            decode_base64_text(Some("IyBE\nZW1v".to_string())).expect("wrapped payload decodes");
        assert_eq!(text, "# Demo");
    }

    #[test]
    fn the_license_listing_picks_the_root_license_first_match() {
        let items: Vec<octocrab::models::repos::Content> =
            serde_json::from_str(&fixture_text("license-listing.json"))
                .expect("fixture is a contents listing");
        let path = license_path_of(items).expect("a license file sits in the listing");
        assert_eq!(
            path, "LICENSE.Custom-1.0",
            "first LICENSE* file, not a dir or a doc copy"
        );
        assert!(license_path_of(Vec::new()).is_none());
    }

    #[test]
    fn the_tree_fixture_maps_kinds_and_carries_truncation() {
        let git_tree: octocrab::models::git::GitTree =
            serde_json::from_str(&fixture_text("tree-page.json"))
                .expect("fixture is a tree response");
        assert!(git_tree.truncated, "the recorded tree is truncated");
        let entries: Vec<TreeEntry> = git_tree.tree.iter().filter_map(tree_entry_of).collect();
        assert_eq!(
            entries.len(),
            4,
            "every recorded kind maps, submodules included"
        );

        let readme = &entries[0];
        assert_eq!(readme.kind, TreeEntryKind::Blob);
        assert_eq!(readme.size, Some(24));
        assert_eq!(entries[1].kind, TreeEntryKind::Tree);
        assert_eq!(entries[1].size, None, "directories carry no size");
        assert_eq!(
            entries[3].kind,
            TreeEntryKind::Commit,
            "the submodule maps as a commit"
        );
    }

    #[test]
    fn unknown_tree_kinds_drop_rather_than_guess() {
        let entry = serde_json::from_value(serde_json::json!({
            "path": "weird", "mode": "000000", "type": "gaia",
            "sha": "abc", "url": "https://api.github.com/x"
        }))
        .expect("a tree entry of an unknown kind");
        assert!(tree_entry_of(&entry).is_none());
    }

    #[test]
    fn the_readme_variant_list_tries_the_documented_order() {
        assert_eq!(
            README_VARIANTS,
            ["README.md", "README.rst", "README", "readme.md"]
        );
    }

    #[test]
    fn absent_errors_are_never_transient() {
        assert!(!is_transient(&ForgeError::Absent("readme")));
        assert!(!is_not_found(&ForgeError::Absent("tree")));
    }

    #[test]
    fn only_http_urls_are_fetchable() {
        assert!(is_fetchable_url(
            "https://raw.githubusercontent.com/o/r/main/a.png"
        ));
        assert!(is_fetchable_url("http://example.org/a.png"));
        assert!(!is_fetchable_url("file:///c:/windows/system32"));
        assert!(!is_fetchable_url("data:image/png;base64,AAAA"));
        assert!(!is_fetchable_url(""));
        assert!(!is_fetchable_url("   "));
    }

    #[test]
    fn the_asset_cap_is_five_megabytes() {
        assert_eq!(ASSET_MAX_BYTES, 5 * 1024 * 1024);
    }

    // --- The digest tarball: the walk over an in-memory tar. The
    // --- fixture tars are built by the tests, never fetched.

    /// The repository the fixture tars pretend to be.
    fn tar_repo() -> RepoRef {
        RepoRef {
            forge: ForgeKind::GitHub,
            owner: "coderamp-labs".to_string(),
            name: "gitingest".to_string(),
        }
    }

    /// The root directory name GitHub's tarballs carry.
    const TAR_SHA: &str = "9f2d1c7e8a5b3f0d6c1e2a4b8d7f0c3e5a1b9d2f";

    /// A gzip'd tar built by `build` in memory: the tar assembled into
    /// a plain buffer, then gzipped whole.
    fn gzipped_tar(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        let tar_bytes = builder.into_inner().expect("tar finished");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzipped");
        encoder.finish().expect("gzip finished")
    }

    /// One file entry under the tarball's root.
    fn add_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, content: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, content)
            .expect("appended");
    }

    /// One directory entry under the tarball's root.
    fn add_dir(builder: &mut tar::Builder<Vec<u8>>, path: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{path}/"), std::io::empty())
            .expect("appended");
    }

    fn walk_bytes(bytes: Vec<u8>, caps: WalkCaps) -> Result<DigestInput, ForgeError> {
        walk_tarball(std::io::Cursor::new(bytes), tar_repo(), caps)
    }

    #[test]
    fn the_walk_maps_files_skips_directories_and_takes_the_root_commit() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            add_dir(builder, &root);
            add_file(builder, &format!("{root}/README.md"), b"# Gitingest\n");
            add_file(
                builder,
                &format!("{root}/src/main.py"),
                b"def main():\n    pass\n",
            );
            add_dir(builder, &format!("{root}/src"));
        });
        let input = walk_bytes(tar, WalkCaps::defaults()).expect("walked");
        assert_eq!(input.repo, tar_repo());
        assert_eq!(
            input.taken_commit.as_deref(),
            Some(TAR_SHA),
            "the sha comes off the root directory name"
        );
        assert!(input.truncated_by.is_none());
        let paths: Vec<&str> = input.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "src/main.py"], "only real files map");
        assert_eq!(input.files[0].bytes, 12);
        assert_eq!(input.files[0].text.as_deref(), Some("# Gitingest\n"));
        assert_eq!(
            input.files[1].text.as_deref(),
            Some("def main():\n    pass\n")
        );
    }

    #[test]
    fn binary_files_are_listed_but_bodyless() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            add_file(
                builder,
                &format!("{root}/logo.png"),
                b"png\x00binary\x00data",
            );
            add_file(builder, &format!("{root}/a.txt"), b"text");
        });
        let input = walk_bytes(tar, WalkCaps::defaults()).expect("walked");
        assert_eq!(input.files.len(), 2, "the binary is listed");
        assert_eq!(input.files[0].path, "logo.png");
        assert_eq!(input.files[0].text, None, "but has no body");
        assert!(input.files[1].text.is_some());
    }

    #[test]
    fn a_file_over_the_per_file_cap_is_listed_without_its_body() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            add_file(builder, &format!("{root}/huge.txt"), &[b'x'; 100]);
            add_file(builder, &format!("{root}/small.txt"), b"tiny");
        });
        let caps = WalkCaps {
            total_bytes: 1_000,
            max_files: 10,
            max_file_bytes: 50,
        };
        let input = walk_bytes(tar, caps).expect("walked");
        assert_eq!(input.files.len(), 2, "the huge file is listed");
        assert_eq!(input.files[0].path, "huge.txt");
        assert_eq!(input.files[0].bytes, 100);
        assert_eq!(input.files[0].text, None, "its body was never read");
        assert!(input.truncated_by.is_none(), "the walk ran to the end");
        assert_eq!(input.files[1].text.as_deref(), Some("tiny"));
    }

    #[test]
    fn the_file_count_cap_stops_the_walk_and_is_reported() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            for index in 0..5 {
                add_file(builder, &format!("{root}/f{index}.txt"), b"one");
            }
        });
        let caps = WalkCaps {
            total_bytes: 1_000,
            max_files: 3,
            max_file_bytes: 100,
        };
        let input = walk_bytes(tar, caps).expect("walked");
        assert_eq!(input.files.len(), 3, "exactly the cap's worth of files");
        assert_eq!(
            input.truncated_by,
            Some(Caps::FileCount { limit: 3 }),
            "the walk names the cap that stopped it"
        );
    }

    #[test]
    fn the_total_bytes_cap_stops_the_walk_and_is_reported() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            add_file(builder, &format!("{root}/a.txt"), &[b'a'; 8]);
            add_file(builder, &format!("{root}/b.txt"), &[b'b'; 8]);
        });
        let caps = WalkCaps {
            total_bytes: 10,
            max_files: 10,
            max_file_bytes: 100,
        };
        let input = walk_bytes(tar, caps).expect("walked");
        assert_eq!(input.files.len(), 1, "the second file would pass the cap");
        assert_eq!(input.files[0].path, "a.txt");
        assert_eq!(
            input.truncated_by,
            Some(Caps::TotalBytes { limit: 10 }),
            "the walk names the cap that stopped it"
        );
    }

    #[test]
    fn the_default_file_count_cap_holds_on_a_monster_tar() {
        let root = format!("coderamp-labs-gitingest-{TAR_SHA}");
        let tar = gzipped_tar(|builder| {
            for index in 0..digest::DEFAULT_MAX_FILES + 1 {
                add_file(builder, &format!("{root}/f{index:05}.txt"), b"z");
            }
        });
        let input = walk_bytes(tar, WalkCaps::defaults()).expect("walked");
        assert_eq!(
            input.files.len(),
            digest::DEFAULT_MAX_FILES,
            "no generation exceeds the cap"
        );
        assert_eq!(
            input.truncated_by,
            Some(Caps::FileCount {
                limit: digest::DEFAULT_MAX_FILES
            })
        );
    }

    #[test]
    fn the_wire_reader_yields_chunks_in_order_and_ends_cleanly() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender.send(Ok(vec![1, 2])).expect("sent");
        sender.send(Ok(vec![3])).expect("sent");
        drop(sender);
        let mut reader = WireReader {
            receiver,
            buffer: Vec::new(),
            position: 0,
        };
        let mut all = Vec::new();
        reader.read_to_end(&mut all).expect("read");
        assert_eq!(all, [1, 2, 3], "chunk boundaries are invisible");
    }

    #[test]
    fn a_wire_transport_failure_becomes_a_read_error() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender.send(Ok(vec![1])).expect("sent");
        sender
            .send(Err("the connection died".to_string()))
            .expect("sent");
        let mut reader = WireReader {
            receiver,
            buffer: Vec::new(),
            position: 0,
        };
        let mut all = Vec::new();
        let error = reader.read_to_end(&mut all).expect_err("the wire failed");
        assert!(error.to_string().contains("the connection died"));
    }

    #[test]
    fn the_tarball_url_points_at_codeload() {
        assert_eq!(
            tarball_url(&tar_repo(), "main"),
            "https://codeload.github.com/coderamp-labs/gitingest/tar.gz/refs/heads/main"
        );
    }

    #[test]
    fn a_root_that_does_not_fit_the_shape_contributes_no_commit() {
        let path = Path::new("pax_global_header");
        assert_eq!(root_commit_of(path, &tar_repo()), None);
        let path = Path::new("someone-else-abcdef1234567890");
        assert_eq!(root_commit_of(path, &tar_repo()), None);
        let path = Path::new("coderamp-labs-gitingest-not-hex");
        assert_eq!(root_commit_of(path, &tar_repo()), None);
    }

    /// The rate-limit stamps injected — the offline stand-in for the
    /// `#[non_exhaustive]` GitHub errors no test can build.
    fn stamps(
        status: u16,
        reset_in: Option<chrono::Duration>,
        retry_after: Option<&str>,
    ) -> LimitStamps<'static> {
        let leaked: Option<&'static str> =
            retry_after.map(|value| &*Box::leak(value.to_string().into_boxed_str()));
        LimitStamps {
            status,
            reset_at: reset_in.map(|delta| Utc::now() + delta),
            retry_after: leaked,
        }
    }

    #[test]
    fn a_primary_limit_names_its_reset_window() {
        let now = Utc::now();
        let pause = rate_pause_stamps(stamps(429, Some(chrono::Duration::minutes(14)), None), now)
            .expect("a 429 with a reset names a wait");
        assert!(
            pause > std::time::Duration::from_secs(13 * 60),
            "the wait runs to the reset: {pause:?}"
        );
    }

    #[test]
    fn a_secondary_limit_names_its_retry_after() {
        let pause = rate_pause_stamps(stamps(403, None, Some("30")), Utc::now())
            .expect("a 403 with Retry-After names a wait");
        assert_eq!(pause, std::time::Duration::from_secs(30));
    }

    #[test]
    fn the_reset_window_wins_when_a_403_carries_both_stamps() {
        let pause = rate_pause_stamps(
            stamps(403, Some(chrono::Duration::hours(1)), Some("30")),
            Utc::now(),
        )
        .expect("stamped");
        assert!(
            pause > std::time::Duration::from_secs(30),
            "the reset ran: {pause:?}"
        );
    }

    #[test]
    fn statuses_that_name_no_wait_are_not_rate_limits() {
        assert_eq!(rate_pause_stamps(stamps(403, None, None), Utc::now()), None);
        // A 429 that names nothing is still a rate limit: the secondary
        // default is the wait.
        assert_eq!(
            rate_pause_stamps(stamps(429, None, None), Utc::now()),
            Some(SECONDARY_PAUSE)
        );
        assert_eq!(
            rate_pause_stamps(stamps(500, None, Some("30")), Utc::now()),
            None
        );
        assert_eq!(rate_pause_stamps(stamps(404, None, None), Utc::now()), None);
    }
}
