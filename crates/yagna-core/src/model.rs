//! The transfer types every crate speaks: one snapshot of a repository and
//! the small records it is built from.
//!
//! Items: `Forge` — which service hosts a repository; `RepoRef` — the
//! identity key (forge, owner, name); `RepoSnapshot` — everything known
//! about one repository; `RepoRow` — the lean page-query projection the
//! list UI renders; `SortKey` — the keys the catalog page can be ordered
//! by; `LanguageStat`, `LicenseInfo`, `ReleaseInfo` — the snapshot's
//! pieces; `TreeData`, `TreeEntry`, `TreeEntryKind` — the folder listing
//! a forge returns; `UserTag` — a user-applied tag; `ImageRef` — a
//! README image and its cache id; `DigestSpec` — what a digest run
//! covers; `RateLimitReport` — what one rate-limit probe saw;
//! `DiscoverQuery`, `DiscoverSort` — the DISCOVER tab's one query shape
//! (five filters: days, stars, language, one topic, one word);
//! `quote_word` — the word's quoting rule;
//! `ShadowRepo`, `MAX_SHADOWS_PER_LOGIN`, `is_followable_login` — the
//! read-only shadows of other users' stars; `LocalRepo`, `LocalEntry`,
//! `LocalEntryKind` — the repositories that live on this machine, as
//! the worktree walk finds them; `NoteSubject` — what a note is about.
//! Depends on: serde (the wire format), chrono (instants), blake3 (asset
//! ids).
//! Called by: crate::health (its input), and the store, forge and app
//! crates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::health::HealthReport;

/// Which service hosts a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Forge {
    GitHub,
}

impl Forge {
    /// The identifier the store keeps in its forge column — the same
    /// string every forge implementation reports through its `id`.
    pub fn as_str(self) -> &'static str {
        match self {
            Forge::GitHub => "github",
        }
    }
}

/// The identity of a repository: forge, owner, name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRef {
    pub forge: Forge,
    pub owner: String,
    pub name: String,
}

/// Everything known about one repository, as the sweep saw it. Optional
/// fields default when absent — the forge legitimately reports nulls.
/// `last_commit_at` equals `created_at` when the repository has no
/// commits: the creation is the newest event it has, and the zero commit
/// count tells the two cases apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoSnapshot {
    pub forge: Forge,
    pub repo_id: u64,
    pub owner: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub languages: Vec<LanguageStat>,
    #[serde(default)]
    pub license: Option<LicenseInfo>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// The default branch's name — the raw-URL base image resolution and
    /// the tree endpoint both need. Empty repositories have none.
    #[serde(default)]
    pub default_branch: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_commit_at: DateTime<Utc>,
    #[serde(default)]
    pub starred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub commit_count: u64,
    #[serde(default)]
    pub latest_release: Option<ReleaseInfo>,
    #[serde(default)]
    pub is_fork: bool,
    #[serde(default)]
    pub is_archived: bool,
}

impl RepoSnapshot {
    /// The repository's identity as a `RepoRef`.
    pub fn repo_ref(&self) -> RepoRef {
        RepoRef {
            forge: self.forge,
            owner: self.owner.clone(),
            name: self.name.clone(),
        }
    }
}

/// One repository's live facts from a single forge call: the snapshot
/// fields the catalog keeps, plus the open-issue count only the live
/// view shows. The snapshot half feeds the catalog's upsert; the
/// `open_issues` half is display-only and is never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoLiveFacts {
    pub snapshot: RepoSnapshot,
    #[serde(default)]
    pub open_issues: u64,
}

/// The keys the catalog page can be ordered by, with the direction each
/// means. Every time key reads newest first; `Age` orders by `created_at`
/// descending, so the youngest repositories come first — the same
/// "most recent first" reading as the other time keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    StarredAt,
    Name,
    Stars,
    Health,
    Age,
    LastCommit,
    Fit,
}

impl SortKey {
    /// The SQL `ORDER BY` fragment for this key: the key's column and
    /// direction, then the stable tiebreak `(forge, owner, name)` —
    /// paging never reshuffles rows whose sort keys are equal. The
    /// fragment is a static string per key, never user input.
    pub fn order_by(self) -> &'static str {
        match self {
            SortKey::StarredAt => {
                "starred_at DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::Name => {
                "name COLLATE NOCASE ASC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::Stars => {
                "stars DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::Health => {
                "health_score DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::Age => {
                "created_at DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::LastCommit => {
                "last_commit_at DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
            SortKey::Fit => {
                "fit_score DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
            }
        }
    }

    /// The label the sort menu shows.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::StarredAt => "starred at",
            SortKey::Name => "name",
            SortKey::Stars => "stars",
            SortKey::Health => "health",
            SortKey::Age => "age",
            SortKey::LastCommit => "last commit",
            SortKey::Fit => "fit",
        }
    }

    /// Every key in menu order.
    pub const ALL: [SortKey; 7] = [
        SortKey::StarredAt,
        SortKey::Name,
        SortKey::Stars,
        SortKey::Health,
        SortKey::Age,
        SortKey::LastCommit,
        SortKey::Fit,
    ];
}

/// One row of the catalog page — the lean projection the list UI
/// renders. Not `RepoSnapshot` (that is the sweep's shape): this is the
/// page query's shape, built by joining the repos row with its README
/// excerpt and its tags. `excerpt` is empty when no README was fetched;
/// `tags` is empty when the user attached none. `repo_rowid` is the
/// catalog's own row id — the key every lazy-layer table (the readme,
/// the assets, the tag attachments) hangs off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoRow {
    #[serde(default)]
    pub repo_rowid: i64,
    pub repo_ref: RepoRef,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub primary_language: Option<String>,
    #[serde(default)]
    pub license_name: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub starred_at: Option<DateTime<Utc>>,
    pub last_commit_at: DateTime<Utc>,
    #[serde(default)]
    pub first_commit_date: Option<DateTime<Utc>>,
    pub health: HealthReport,
    /// The fit score beside the pinned health — `None` until a re-score
    /// writes one.
    #[serde(default)]
    pub fit_score: Option<f64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub excerpt: String,
}

/// One language's share of the code tree: byte size and the color the
/// service paints it with (absent when the service has none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageStat {
    pub name: String,
    pub bytes: u64,
    #[serde(default)]
    pub color: Option<String>,
}

/// One repository's detail view: every field the repository tab shows,
/// read out of the catalog in one query. It is the snapshot's shape
/// plus the two things the sweep does not carry — the first commit's
/// *source* and the row id every lazy-layer table hangs off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoDetail {
    #[serde(default)]
    pub repo_rowid: i64,
    pub repo_ref: RepoRef,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub languages: Vec<LanguageStat>,
    #[serde(default)]
    pub license: Option<LicenseInfo>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_commit_at: DateTime<Utc>,
    #[serde(default)]
    pub starred_at: Option<DateTime<Utc>>,
    /// The first commit's date — the creation's, until the exact
    /// enrichment replaces it.
    #[serde(default)]
    pub first_commit_date: Option<DateTime<Utc>>,
    /// Where that date came from: `repo-created` or `git-history`.
    #[serde(default)]
    pub first_commit_source: Option<String>,
    #[serde(default)]
    pub commit_count: u64,
    #[serde(default)]
    pub latest_release: Option<ReleaseInfo>,
    #[serde(default)]
    pub is_fork: bool,
    #[serde(default)]
    pub is_archived: bool,
    pub health: HealthReport,
    /// The fit score's stored breakdown beside the health one — `None`
    /// until a re-score writes one.
    #[serde(default)]
    pub fit: Option<crate::fit::FitReport>,
}

/// A repository's license: its display name and its SPDX id — absent for
/// non-standard texts, and `"NOASSERTION"` when the service can name the
/// license family but not an SPDX entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseInfo {
    pub name: String,
    #[serde(default)]
    pub spdx_id: Option<String>,
}

/// The latest release of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag: String,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
}

/// The folder listing of a repository's default branch, as the forge saw
/// it. `truncated` repeats the service's own flag: the listing may be
/// incomplete, and the fact is kept, never swallowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeData {
    pub entries: Vec<TreeEntry>,
    #[serde(default)]
    pub truncated: bool,
}

/// One path in a tree: its full path from the repository root, whether it
/// is a file or a directory (or a submodule), and its byte size — absent
/// for directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub kind: TreeEntryKind,
    #[serde(default)]
    pub size: Option<u64>,
}

/// What a tree path is: a file (`blob`), a directory (`tree`), or a
/// submodule (`commit`) on the hosting service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeEntryKind {
    Blob,
    Tree,
    Commit,
}

/// A tag the user attached to one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserTag {
    pub repo: RepoRef,
    pub tag: String,
}

/// A README image: its source URL and the content-hash id under which the
/// asset cache keeps its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    pub url: String,
    pub asset_id: String,
}

impl ImageRef {
    /// Derives the cache id from the source URL: the same URL always
    /// produces the same id, so link rewrites stay deterministic.
    pub fn from_url(url: impl Into<String>) -> Self {
        let url = url.into();
        let asset_id = blake3::hash(url.as_bytes()).to_hex().to_string();
        Self { url, asset_id }
    }
}

/// What a digest run covers: the repository, the include and exclude glob
/// patterns, and the per-file size cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestSpec {
    pub repo: RepoRef,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
}

/// What one rate-limit probe saw: the core REST budget and the search
/// budget, limit and remainder — and the instant the core budget
/// resets, when the endpoint named one. The reset is what a runner
/// under the floor counts its visible pause down to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitReport {
    pub core_limit: u64,
    pub core_remaining: u64,
    pub search_limit: u64,
    pub search_remaining: u64,
    #[serde(default)]
    pub core_reset: Option<DateTime<Utc>>,
}

/// How the DISCOVER tab sorts its one query: stars or recency — the
/// OhNiceRepo ceiling, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoverSort {
    Stars,
    Newest,
}

impl DiscoverSort {
    /// The search API's sort parameter.
    pub fn param(self) -> &'static str {
        match self {
            DiscoverSort::Stars => "stars",
            DiscoverSort::Newest => "created",
        }
    }

    /// The label the DISCOVER controls show.
    pub fn label(self) -> &'static str {
        match self {
            DiscoverSort::Stars => "stars",
            DiscoverSort::Newest => "newest",
        }
    }

    /// Every sort the DISCOVER controls offer, in menu order.
    pub const ALL: [DiscoverSort; 2] = [DiscoverSort::Stars, DiscoverSort::Newest];
}

/// One DISCOVER query: created within the last N days, above a star
/// floor, optionally in one language, optionally about one word in one
/// topic, sorted one of two ways. One query shape, five filters — the
/// OhNiceRepo ceiling (one request, one page) stands; the word and the
/// topic are the amendment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverQuery {
    #[serde(default = "DiscoverQuery::default_days")]
    pub created_within_days: u32,
    #[serde(default = "DiscoverQuery::default_min_stars")]
    pub min_stars: u32,
    #[serde(default)]
    pub language: Option<String>,
    /// One topic — GitHub's search takes one `topic:` qualifier per
    /// query, so the field is one and the surface says so.
    #[serde(default)]
    pub topic: Option<String>,
    /// The word the repositories' names and descriptions should carry.
    #[serde(default)]
    pub word: Option<String>,
    #[serde(default = "DiscoverQuery::default_sort")]
    pub sort: DiscoverSort,
}

/// The characters GitHub's search reads as query syntax: a word
/// carrying any of them is quoted, so a word stays a word and never
/// becomes a filter by accident.
const QUERY_SYNTAX_CHARS: [char; 4] = [':', '"', '>', '<'];

/// One DISCOVER word as the search API reads it: wrapped in quotes
/// when it carries a space or a query-syntax character, bare
/// otherwise. The one implementation — the surface's label, the live
/// URL and the agent's tool all show the word through it.
pub fn quote_word(word: &str) -> String {
    let is_syntax = word
        .chars()
        .any(|c| c.is_whitespace() || QUERY_SYNTAX_CHARS.contains(&c));
    if is_syntax {
        format!("\"{word}\"")
    } else {
        word.to_string()
    }
}

impl DiscoverQuery {
    fn default_days() -> u32 {
        30
    }

    fn default_min_stars() -> u32 {
        200
    }

    fn default_sort() -> DiscoverSort {
        DiscoverSort::Stars
    }

    /// The search API's `q` value against `now`: the created-after floor
    /// is a calendar date, so the query is a pure function of the
    /// session instant.
    pub fn query_text(&self, now: DateTime<Utc>) -> String {
        use chrono::Duration;
        let floor = (now - Duration::days(i64::from(self.created_within_days.max(1)))).date_naive();
        let mut query = format!("created:>{floor} stars:>{}", self.min_stars);
        if let Some(language) = self
            .language
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            query.push_str(&format!(" language:{language}"));
        }
        if let Some(topic) = self
            .topic
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            query.push_str(&format!(" topic:{topic}"));
        }
        if let Some(word) = self
            .word
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            query.push(' ');
            query.push_str(&quote_word(word));
        }
        query
    }
}

impl Default for DiscoverQuery {
    fn default() -> Self {
        Self {
            created_within_days: Self::default_days(),
            min_stars: Self::default_min_stars(),
            language: Some("rust".to_string()),
            topic: None,
            word: None,
            sort: Self::default_sort(),
        }
    }
}

/// One shadow row: another user's starred repository as a listable
/// fact. A shadow is browsable only — no tags, no health, no
/// enrichment beyond what the row shows. The catalog's own tables
/// never answer a shadow; the distinction is structural (separate
/// tables), never a flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowRepo {
    pub login: String,
    pub snapshot: RepoSnapshot,
    pub fetched_at: DateTime<Utc>,
}

impl ShadowRepo {
    /// The row's triple for the PEOPLE list: name, languages, stars —
    /// the pitch's triple, nothing more.
    pub fn triple(&self) -> (String, Vec<String>, u64) {
        let name = format!("{}/{}", self.snapshot.owner, self.snapshot.name);
        let languages = self
            .snapshot
            .languages
            .iter()
            .map(|language| language.name.clone())
            .collect();
        (name, languages, self.snapshot.stars)
    }
}

/// The most shadow rows kept per followed login: views, not history —
/// the oldest go when the cap bites.
pub const MAX_SHADOWS_PER_LOGIN: usize = 500;

/// Whether a login may be followed: GitHub's username rules, loosely —
/// alphanumerics and hyphens, never blank, never overlong.
pub fn is_followable_login(login: &str) -> bool {
    let trimmed = login.trim();
    if trimmed.is_empty() || trimmed.len() > 39 {
        return false;
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut previous_hyphen = !first.is_ascii_alphanumeric() || first == '-';
    for char in chars {
        if char == '-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if char.is_ascii_alphanumeric() {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    !previous_hyphen
}

/// One repository that lives on this machine: where it is, what its
/// origin says about it, and what its worktree holds. A local entry is
/// a fact about the disk, never about the service — it stands beside
/// the catalog's row of the same repository, not in place of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalRepo {
    /// The entry's own id — the key the local table and the notes hang
    /// off.
    pub id: i64,
    /// The folder the scan was pointed at: the root the entry was found
    /// under. Re-pointing the folder at another path leaves the old
    /// entries standing, marked missing.
    pub root: String,
    /// The worktree's absolute path.
    pub path: String,
    /// The directory's own name.
    pub name: String,
    /// The owner the origin names — `None` when the origin says
    /// nothing the catalog can match.
    pub owner: Option<String>,
    /// The repository name the origin names.
    pub repo_name: Option<String>,
    /// The origin URL, as the worktree's own config carries it.
    pub origin: Option<String>,
    /// The branch the worktree is on.
    pub branch: Option<String>,
    /// The last commit's date, as the worktree's own reflog records it.
    /// `None` when the worktree records no date — the app says so
    /// rather than guessing one.
    pub last_commit_at: Option<DateTime<Utc>>,
    /// How many files the worktree holds.
    pub file_count: u64,
    /// How many bytes those files hold.
    pub total_bytes: u64,
    /// The catalog row this entry matches — `None` when the catalog has
    /// no row for it, which makes the entry standalone.
    pub linked_rowid: Option<i64>,
    /// When the folder went missing — `None` while it is there.
    pub missing_at: Option<DateTime<Utc>>,
    /// When the scan that found it ran.
    pub scanned_at: DateTime<Utc>,
}

/// One entry of a browsed directory: its name, its path, and whether it
/// is a folder or a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEntry {
    /// The entry's own name.
    pub name: String,
    /// The entry's path, relative to the worktree's root.
    pub path: String,
    /// Whether the entry is a folder or a file.
    pub kind: LocalEntryKind,
    /// The file's size in bytes — `None` for a folder.
    pub size: Option<u64>,
}

/// What a browsed entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalEntryKind {
    /// A folder.
    Directory,
    /// A file.
    File,
}

/// What a note is about: a row of the catalog, or a repository that
/// lives on this machine. The kinds are separate subjects behind one
/// key, so a note is written and read the same way whichever it
/// belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteSubject {
    /// A row of the catalog — `repos.id`.
    Repo(i64),
    /// A local entry — `local_repos.id`.
    Local(i64),
}

impl NoteSubject {
    /// The subject's kind, as the notes table spells it.
    pub fn kind(self) -> &'static str {
        match self {
            NoteSubject::Repo(_) => "repo",
            NoteSubject::Local(_) => "local",
        }
    }

    /// The subject's own id.
    pub fn id(self) -> i64 {
        match self {
            NoteSubject::Repo(id) | NoteSubject::Local(id) => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::health::HealthReport;

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn sample_snapshot() -> RepoSnapshot {
        RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1_040_000,
            owner: "someone".to_string(),
            name: "something".to_string(),
            description: Some("a catalog".to_string()),
            stars: 1_234,
            languages: vec![LanguageStat {
                name: "Rust".to_string(),
                bytes: 540_000,
                color: Some("#dea584".to_string()),
            }],
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: vec!["rust".to_string(), "search".to_string()],
            default_branch: Some("main".to_string()),
            created_at: date(2020, 1, 2),
            last_commit_at: date(2026, 10, 1),
            starred_at: Some(date(2026, 10, 20)),
            commit_count: 4_321,
            latest_release: Some(ReleaseInfo {
                tag: "v1.2.3".to_string(),
                published_at: Some(date(2026, 9, 9)),
            }),
            is_fork: false,
            is_archived: false,
        }
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let snapshot = sample_snapshot();
        let text = serde_json::to_string(&snapshot).expect("serialize");
        let back: RepoSnapshot = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(snapshot, back);
    }

    #[test]
    fn snapshot_defaults_every_optional_field() {
        let value = json!({
            "forge": "github",
            "repo_id": 7,
            "owner": "someone",
            "name": "something",
            "created_at": "2020-01-02T12:00:00Z",
            "last_commit_at": "2026-10-01T12:00:00Z",
        });
        let snapshot: RepoSnapshot = serde_json::from_value(value).expect("deserialize");
        assert_eq!(snapshot.description, None);
        assert_eq!(snapshot.stars, 0);
        assert!(snapshot.languages.is_empty());
        assert_eq!(snapshot.license, None);
        assert!(snapshot.topics.is_empty());
        assert_eq!(snapshot.default_branch, None);
        assert_eq!(snapshot.starred_at, None);
        assert_eq!(snapshot.commit_count, 0);
        assert_eq!(snapshot.latest_release, None);
        assert!(!snapshot.is_fork);
        assert!(!snapshot.is_archived);
    }

    #[test]
    fn forge_renders_its_store_identifier() {
        assert_eq!(Forge::GitHub.as_str(), "github");
    }

    #[test]
    fn tree_data_roundtrips_through_json() {
        let tree = TreeData {
            entries: vec![
                TreeEntry {
                    path: "src".to_string(),
                    kind: TreeEntryKind::Tree,
                    size: None,
                },
                TreeEntry {
                    path: "src/main.rs".to_string(),
                    kind: TreeEntryKind::Blob,
                    size: Some(4_096),
                },
            ],
            truncated: false,
        };
        let text = serde_json::to_string(&tree).expect("serialize");
        let back: TreeData = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(tree, back);
        let bare: TreeData = serde_json::from_str(r#"{"entries":[]}"#).expect("truncated defaults");
        assert!(!bare.truncated);
    }

    #[test]
    fn image_ref_ids_are_deterministic() {
        let first = ImageRef::from_url("https://example.com/a.png");
        let second = ImageRef::from_url("https://example.com/a.png");
        let other = ImageRef::from_url("https://example.com/b.png");
        assert_eq!(first, second);
        assert_ne!(first.asset_id, other.asset_id);
        assert!(!first.asset_id.is_empty());
    }

    #[test]
    fn tags_and_digest_specs_roundtrip() {
        let tag = UserTag {
            repo: RepoRef {
                forge: Forge::GitHub,
                owner: "someone".to_string(),
                name: "something".to_string(),
            },
            tag: "reading list".to_string(),
        };
        let spec = DigestSpec {
            repo: tag.repo.clone(),
            include: vec!["src/**".to_string()],
            exclude: vec![],
            max_file_bytes: Some(64_000),
        };
        let tag_text = serde_json::to_string(&tag).expect("serialize tag");
        let tag_back: UserTag = serde_json::from_str(&tag_text).expect("deserialize tag");
        assert_eq!(tag, tag_back);
        let spec_text = serde_json::to_string(&spec).expect("serialize spec");
        let spec_back: DigestSpec = serde_json::from_str(&spec_text).expect("deserialize spec");
        assert_eq!(spec, spec_back);
    }

    #[test]
    fn rate_limit_report_roundtrips() {
        let report = RateLimitReport {
            core_limit: 5_000,
            core_remaining: 4_999,
            search_limit: 30,
            search_remaining: 29,
            core_reset: Some(date(2026, 10, 30)),
        };
        let text = serde_json::to_string(&report).expect("serialize");
        let back: RateLimitReport = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(report, back);
    }

    #[test]
    fn repo_row_roundtrips_through_json() {
        let row = RepoRow {
            repo_rowid: 7,
            repo_ref: RepoRef {
                forge: Forge::GitHub,
                owner: "someone".to_string(),
                name: "something".to_string(),
            },
            description: Some("a catalog".to_string()),
            stars: 1_234,
            primary_language: Some("Rust".to_string()),
            license_name: Some("MIT License".to_string()),
            topics: vec!["rust".to_string(), "search".to_string()],
            created_at: date(2020, 1, 2),
            starred_at: Some(date(2026, 10, 20)),
            last_commit_at: date(2026, 10, 1),
            first_commit_date: Some(date(2020, 1, 2)),
            health: HealthReport {
                total: 9,
                age: 3,
                stars: 2,
                maintenance: 2,
                cadence: 1,
                license: 2,
                version: 0,
            },
            fit_score: Some(4.5),
            tags: vec!["reading list".to_string()],
            excerpt: "two italic lines of README".to_string(),
        };
        let text = serde_json::to_string(&row).expect("serialize");
        let back: RepoRow = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(row, back);
    }

    #[test]
    fn repo_row_defaults_its_optional_fields() {
        let value = json!({
            "repo_ref": {
                "forge": "github",
                "owner": "someone",
                "name": "something",
            },
            "created_at": "2020-01-02T12:00:00Z",
            "last_commit_at": "2026-10-01T12:00:00Z",
            "health": HealthReport::default(),
        });
        let row: RepoRow = serde_json::from_value(value).expect("deserialize");
        assert_eq!(row.repo_rowid, 0, "the row id defaults to zero");
        assert_eq!(row.description, None);
        assert_eq!(row.stars, 0);
        assert_eq!(row.primary_language, None);
        assert_eq!(row.license_name, None);
        assert!(row.topics.is_empty());
        assert_eq!(row.starred_at, None);
        assert_eq!(row.first_commit_date, None);
        assert!(row.tags.is_empty());
        assert_eq!(row.excerpt, "");
    }

    #[test]
    fn sort_keys_serialize_as_kebab_case() {
        let text = serde_json::to_string(&SortKey::StarredAt).expect("serialize");
        assert_eq!(text, r#""starred-at""#);
        let back: SortKey = serde_json::from_str(r#""last-commit""#).expect("deserialize");
        assert_eq!(back, SortKey::LastCommit);
    }

    #[test]
    fn discover_query_builds_the_ohnice_query_shape() {
        let query = DiscoverQuery {
            created_within_days: 30,
            min_stars: 200,
            language: Some("rust".to_string()),
            topic: None,
            word: None,
            sort: DiscoverSort::Stars,
        };
        let text = query.query_text(date(2026, 10, 31));
        assert_eq!(text, "created:>2026-10-01 stars:>200 language:rust");
        let bare = DiscoverQuery {
            language: None,
            ..query.clone()
        };
        assert_eq!(
            bare.query_text(date(2026, 10, 31)),
            "created:>2026-10-01 stars:>200"
        );
        assert_eq!(DiscoverSort::Stars.param(), "stars");
        assert_eq!(DiscoverSort::Newest.param(), "created");
    }

    #[test]
    fn the_query_shape_carries_the_topic_and_the_word_in_order() {
        let query = DiscoverQuery {
            topic: Some("wasm".to_string()),
            word: Some("parser".to_string()),
            ..DiscoverQuery::default()
        };
        let text = query.query_text(date(2026, 10, 31));
        assert_eq!(
            text,
            "created:>2026-10-01 stars:>200 language:rust topic:wasm parser"
        );
    }

    #[test]
    fn a_word_carrying_query_syntax_is_quoted() {
        assert_eq!(quote_word("parser"), "parser", "a plain word stays bare");
        assert_eq!(quote_word("web server"), "\"web server\"", "a space quotes");
        assert_eq!(quote_word("c:lang"), "\"c:lang\"", "a colon quotes");
        assert_eq!(
            quote_word("say \"hi\""),
            "\"say \"hi\"\"",
            "a quote quotes — the wrap is the whole rule; what GitHub \
             makes of the inner quotes is GitHub's reading"
        );
        assert_eq!(
            quote_word("older>2020"),
            "\"older>2020\"",
            "an angle quotes"
        );
        assert_eq!(quote_word("under<100"), "\"under<100\"", "an angle quotes");
        assert_eq!(
            quote_word("  spaced  "),
            "\"  spaced  \"",
            "the word is quoted as it stands; trimming is the caller's trim"
        );
    }

    #[test]
    fn a_blank_topic_or_word_leaves_the_query_unchanged() {
        let query = DiscoverQuery {
            topic: Some("   ".to_string()),
            word: Some(" \t ".to_string()),
            ..DiscoverQuery::default()
        };
        assert_eq!(
            query.query_text(date(2026, 10, 31)),
            "created:>2026-10-01 stars:>200 language:rust",
            "blanks ride as absent"
        );
    }

    #[test]
    fn a_query_written_before_the_word_and_topic_parses_back() {
        let old = r#"{"created_within_days":30,"min_stars":200,"language":"rust","sort":"stars"}"#;
        let query: DiscoverQuery = serde_json::from_str(old).expect("the old shape parses");
        assert_eq!(query, DiscoverQuery::default());
        assert_eq!(query.topic, None);
        assert_eq!(query.word, None);
    }

    #[test]
    fn followable_logins_reject_blanks_and_double_hyphens() {
        assert!(is_followable_login("octocat"));
        assert!(is_followable_login("a-b"));
        assert!(!is_followable_login(""));
        assert!(!is_followable_login("-lead"));
        assert!(!is_followable_login("trail-"));
        assert!(!is_followable_login("two--hyphens"));
        assert!(!is_followable_login("has space"));
    }

    #[test]
    fn shadow_triple_names_languages_and_stars() {
        let mut snapshot = sample_snapshot();
        snapshot.owner = "other".to_string();
        snapshot.name = "thing".to_string();
        snapshot.stars = 42;
        let shadow = ShadowRepo {
            login: "friend".to_string(),
            snapshot,
            fetched_at: date(2026, 10, 30),
        };
        let (name, languages, stars) = shadow.triple();
        assert_eq!(name, "other/thing");
        assert_eq!(languages, ["Rust".to_string()]);
        assert_eq!(stars, 42);
        assert_eq!(MAX_SHADOWS_PER_LOGIN, 500);
    }

    #[test]
    fn the_fit_sort_key_orders_by_the_fit_score_with_the_same_tiebreak() {
        assert_eq!(
            SortKey::Fit.order_by(),
            "fit_score DESC, forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"
        );
        assert_eq!(SortKey::Fit.label(), "fit");
        assert_eq!(SortKey::ALL.len(), 7);
    }

    #[test]
    fn every_sort_key_orders_its_column_then_the_tiebreak() {
        for key in SortKey::ALL {
            let order = key.order_by();
            let column = match key {
                SortKey::StarredAt => "starred_at",
                SortKey::Name => "name",
                SortKey::Stars => "stars",
                SortKey::Health => "health_score",
                SortKey::Age => "created_at",
                SortKey::LastCommit => "last_commit_at",
                SortKey::Fit => "fit_score",
            };
            assert!(
                order.starts_with(column),
                "{key:?} must order by its own column first: {order}"
            );
            assert!(
                order.ends_with("forge ASC, owner COLLATE NOCASE ASC, name COLLATE NOCASE ASC"),
                "{key:?} must end in the stable tiebreak: {order}"
            );
        }
    }
}
