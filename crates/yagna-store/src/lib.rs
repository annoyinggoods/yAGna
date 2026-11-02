//! The disk world of yAGna. The SQLite catalog, the asset cache and the
//! work queues live here; the catalog (repos, tags, sync runs), the
//! enrichment tables (readmes, license texts, trees, assets), the digest
//! table with its full-text indexes, the two work queues, and the
//! read-only shadows (other users' stars, the DISCOVER results) are
//! what exist today.
//!
//! `Store` is the only way in: it opens or creates the database file it
//! is handed and walks `user_version` through the migration list. The
//! catalog module writes and reads the repos table — the sweep's upserts
//! and the read side the list UI renders through (`page`, `total_count`);
//! the runs module keeps the sync ledger; the enrichment module stores
//! the lazy layer and works the queue; the digests module stores the
//! derived digest artifact and keeps its FTS row in step; the search
//! module runs the deep FTS queries over READMEs and digests; the
//! appstate module keeps the app's own key-value state (the persisted
//! expansion set, the eviction counters); the userstats module holds
//! the USER tab's one-SQL-aggregate queries; the export module reads
//! the catalog out in the export shape and merges an export back in;
//! the notes module holds the reader's notes (one per subject: a
//! catalog row or a local entry); the local module finds the
//! repositories that live on this machine and reads their worktrees.
//! No network, no UI — a gate test keeps them out.
//!
//! Items: `Store` — the facade over one open database; `StoreError` —
//! the crate's errors; `MIGRATIONS` — one SQL script per schema version;
//! `catalog` — the repos table (upserts, missing-marking, counts, the
//! `page` query, the search-index read); `runs` — the sync_runs ledger;
//! `enrichment` — the lazy layer (`QueueKind`, `QueueItem`,
//! `QueueDepths`, `store_readme` and friends); `digests` — the digest
//! artifact (`DigestBody`, `store_digest`, the stale stamping, the
//! per-repository enqueue); `search` — `Store::search` and its
//! `SearchHit`s; `shadows` — the read-only shadows and the followed
//! list; `tags` — the user's tags and categories; `notes` — the
//! reader's notes, one per subject; `local` — the repositories that
//! live on this machine (the folder scan, the worktree reads, the
//! local table); `assets` —
//! the asset cache's landing side (the preview asset, the asset queue,
//! the README head, the cap's prune); `export` — the export and import
//! (the export shape's read and merge, the SQL backup).
//! Depends on: rusqlite (the bundled SQLite), serde_json (the JSON
//! columns), chrono (timestamps), yagna_core (the DTOs it persists, the
//! markdown and digest math, the query AST, the export shape).
//! Called by: the app crate.

use std::path::Path;

use rusqlite::Connection;

mod appstate;
mod assets;
mod catalog;
mod detail;
mod digests;
mod enrichment;
mod export;
mod local;
mod notes;
mod runs;
mod search;
mod shadows;
mod tags;
mod userstats;

pub use appstate::{
    EVICTED_BYTES_KEY, EVICTED_FILES_KEY, EXPANDED_ROWS_KEY, FIT_RESCORED_AT_KEY,
    FIT_RESCORED_MOVED_KEY, FIT_RESCORED_ROWS_KEY, SIDEBAR_DRAWER_KEY,
};
pub use assets::{
    ASSET_EXTENSION, AssetJob, AssetRecord, CacheBreakdown, CacheStats, PruneReport, ReadmeHead,
    cache_stats,
};
pub use catalog::{
    CatalogFilter, CatalogOverview, FIRST_COMMIT_SOURCE_DEFAULT, RescoreReport, SearchRow,
    TagMatch, topic_counts,
};
pub use detail::{
    FIRST_COMMIT_SOURCE_GIT_HISTORY, ImagePolicy, PrefetchPlan, PrefetchScope, ReadmeBody, TreeBody,
};
pub use digests::DigestBody;
pub use enrichment::{
    LEASE_SECS, QueueDepths, QueueItem, QueueKind, README_MAX_BYTES, RETRY_BACKOFF_SECS,
};
pub use export::ImportReport;
pub use local::{LocalError, LocalPreview, LocalScan, read_directory, read_preview, scan_folder};
pub use notes::NoteRecord;
pub use runs::{KIND_FULL, SyncRunRecord};
pub use search::{SearchHit, SearchSource};
pub use shadows::DISCOVER_LOGIN;
pub use tags::{TagKind, TagRecord};
pub use userstats::{FitSpread, HealthBuckets, LanguageStars, NameCount, YearCount};

/// One SQL script per schema version: `MIGRATIONS[n]` upgrades a database
/// at version `n` to `n + 1`. Plain SQL files, embedded — no ad-hoc
/// schema change ever happens outside one.
pub const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001-catalog.sql"),
    include_str!("migrations/0002-enrichment.sql"),
    include_str!("migrations/0003-tags.sql"),
    include_str!("migrations/0004-asset-queue.sql"),
    include_str!("migrations/0005-digests.sql"),
    include_str!("migrations/0006-app-state.sql"),
    include_str!("migrations/0007-queue-leases.sql"),
    include_str!("migrations/0008-claim-tokens.sql"),
    include_str!("migrations/0009-readme-assets.sql"),
    include_str!("migrations/0010-shadows.sql"),
    include_str!("migrations/0011-notes.sql"),
    include_str!("migrations/0012-local-repos.sql"),
    include_str!("migrations/0013-fit-score.sql"),
];

/// The errors the store can raise.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("catalog json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("stored data is not what the schema promises: {0}")]
    Parse(String),
    #[error("schema version {found} is newer than this build knows ({known}); upgrade yAGna first")]
    SchemaTooNew { found: u32, known: u32 },
    #[error("digests are never pre-generated — they are enqueued per repository")]
    DigestOnDemand,
}

/// The facade over one open database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    /// The file this store was opened from — the footprint probe and
    /// the WAL-size line read beside it (`-wal`, `-shm`).
    path: std::path::PathBuf,
}

impl Store {
    /// Opens the database file at `path`, creating it when absent, and
    /// applies any pending migrations. The parent directory must exist.
    /// The connection runs WAL (the big syncs' write pattern wants it;
    /// a filesystem that refuses the mode runs on as it is) with a
    /// five-second busy timeout, so a second writer's lock is waited
    /// out, not errored on.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        migrate(&conn)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// The schema version this database sits at.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?)
    }

    /// Checkpoints the write-ahead log back into the database file and
    /// truncates it — the queue drains' closing move, so a big run
    /// does not leave a WAL file the size of its work behind.
    pub fn checkpoint_wal(&self) -> Result<(), StoreError> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(())
    }

    /// The database's footprint: the file's size on disk, the WAL
    /// file's size, and the page/freelist counts — the stats verb's
    /// maintenance lines.
    pub fn footprint(&self) -> Result<DbFootprint, StoreError> {
        let page_size: i64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let pages: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let freelist: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let file_bytes = std::fs::metadata(&self.path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let mut wal_bytes = 0;
        let wal_path = wal_sibling(&self.path);
        if let Ok(meta) = std::fs::metadata(&wal_path) {
            wal_bytes = meta.len();
        }
        Ok(DbFootprint {
            file_bytes,
            wal_bytes,
            pages: pages.max(0) as u64,
            freelist_pages: freelist.max(0) as u64,
            page_size: page_size.max(0) as u64,
        })
    }

    /// The per-object byte sizes from SQLite's `dbstat` virtual table,
    /// largest first — when this bundled SQLite carries it (a build
    /// without `dbstat` yields `None` and the stats line skips the
    /// breakdown).
    pub fn dbstat_sizes(&self) -> Option<Vec<(String, u64)>> {
        let mut statement = self
            .conn
            .prepare("SELECT name, SUM(pgsize) FROM dbstat GROUP BY name ORDER BY 2 DESC")
            .ok()?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })
            .ok()?;
        let mut sizes: Vec<(String, u64)> = rows.collect::<Result<_, _>>().ok()?;
        sizes.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        Some(sizes)
    }
}

/// The WAL file that belongs to one database file: its path plus the
/// `-wal` suffix.
fn wal_sibling(path: &Path) -> std::path::PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push("-wal");
    path.with_file_name(name)
}

/// The database's footprint on disk, as the stats verb reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DbFootprint {
    pub file_bytes: u64,
    pub wal_bytes: u64,
    pub pages: u64,
    pub freelist_pages: u64,
    pub page_size: u64,
}

/// Rowids and queue ids are u64 at the seam and i64 in SQLite; the
/// counts fit by far, and a value that would not clamps rather than
/// wraps. One helper for every module of the crate.
pub(crate) fn saturating_rowid(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// An immediate transaction over a shared connection: the write lock is
/// taken up front, so two processes claiming or upserting against one
/// database serialize here instead of racing a deferred read snapshot
/// into BUSY_SNAPSHOT at the write upgrade (which the busy timeout
/// does not retry). rusqlite's own immediate transaction wants `&mut`;
/// the store's claim paths run under one outer lock and take `&self` —
/// this is the same BEGIN, by hand.
pub(crate) struct ImmediateTx<'a> {
    conn: &'a Connection,
}

impl<'a> ImmediateTx<'a> {
    /// Begins. A busy database waits out the busy timeout here — that
    /// is the serialization.
    pub(crate) fn begin(conn: &'a Connection) -> Result<Self, StoreError> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self { conn })
    }

    /// Commits. Consuming `self` keeps the drop-side rollback away.
    pub(crate) fn commit(self) -> Result<(), StoreError> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
}

impl Drop for ImmediateTx<'_> {
    fn drop(&mut self) {
        // A committed transaction is gone; this rollback is the
        // error-path cleanup and a no-op after a commit.
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

/// Walks the migration list, bumping `user_version` after each script. A
/// database newer than the known list is refused, never downgraded.
fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let current = read_user_version(conn)?;
    let known = MIGRATIONS.len() as u32;
    if current > known {
        return Err(StoreError::SchemaTooNew {
            found: current,
            known,
        });
    }
    for index in current..known {
        apply_migration(conn, index)?;
    }
    Ok(())
}

/// Applies `MIGRATIONS[index]` and stamps the version one past it, in
/// one transaction: a crash between the script and the stamp must not
/// leave a half-migrated database whose next open re-runs the script
/// into "table already exists" and refuses to open at all. A failed
/// script rolls back whole (`PRAGMA user_version` is transactional in
/// SQLite).
fn apply_migration(conn: &Connection, index: u32) -> Result<(), StoreError> {
    let batch = format!(
        "BEGIN IMMEDIATE;\n{}\nPRAGMA user_version = {};\nCOMMIT;",
        MIGRATIONS[index as usize],
        index + 1
    );
    let applied = conn.execute_batch(&batch);
    if let Err(error) = applied {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error.into());
    }
    Ok(())
}

fn read_user_version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?)
}

#[cfg(test)]
pub(crate) mod testing {
    //! Scratch directories and sample data shared by the store's tests:
    //! every test works in its own temp directory, removed on drop —
    //! never in the repository.

    use std::fs;
    use std::path::PathBuf;

    use yagna_core::model::{Forge, LicenseInfo, RepoSnapshot};

    pub(crate) struct Scratch(PathBuf);

    impl Scratch {
        pub(crate) fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("yagna-store-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch directory");
            Self(dir)
        }

        pub(crate) fn path(&self) -> PathBuf {
            self.0.join("test.db")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A licensed, branched, recently-committed snapshot — the shape the
    /// queue and storage tests seed with.
    pub(crate) fn sample_snapshot(owner: &str, name: &str) -> RepoSnapshot {
        use chrono::TimeZone;
        use chrono::Utc;
        let at = |year: i32, month: u32, day: u32| {
            Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
                .single()
                .expect("valid test date")
        };
        RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: owner.to_string(),
            name: name.to_string(),
            description: Some("a repo".to_string()),
            stars: 10,
            languages: Vec::new(),
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: Vec::new(),
            default_branch: Some("main".to_string()),
            created_at: at(2020, 5, 1),
            last_commit_at: at(2026, 9, 1),
            starred_at: Some(at(2026, 10, 1)),
            commit_count: 500,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn open_creates_the_database_and_reports_the_catalog_version() {
        let scratch = Scratch::new("create");
        let path = scratch.path();
        assert!(!path.exists());
        let store = Store::open(&path).expect("open");
        assert!(path.exists());
        assert_eq!(store.schema_version().expect("version"), 13);
        drop(store);
        let reopened = Store::open(&scratch.path()).expect("reopen");
        assert_eq!(reopened.schema_version().expect("version"), 13);
    }

    #[test]
    fn rescore_fit_stores_the_score_and_breakdown() {
        use chrono::Utc;
        use yagna_core::fit::{default_spec, score};
        let scratch = Scratch::new("rescore-fit");
        let mut store = Store::open(&scratch.path()).expect("open");
        let now = Utc::now();
        let snapshot = testing::sample_snapshot("owner", "repo");
        store.upsert_snapshots(&[snapshot], now).expect("upsert");

        let spec = default_spec();
        let report = store.rescore_fit(&spec, now).expect("rescore");
        assert_eq!(report.scored, 1);

        let rowid = store
            .repo_rowid("github", "owner", "repo")
            .expect("lookup")
            .expect("row exists");
        let facts = store.fit_facts_for_repo(rowid, now).expect("facts");
        let expected = score(&spec, &facts);
        let stored = store.fit_score_of(rowid).expect("read fit");
        assert!(stored.is_some(), "fit_score is stored");
        let (fit_score, fit_json) = stored.expect("fit");
        assert!((fit_score - expected.total).abs() < 1e-9);
        let breakdown: yagna_core::fit::FitReport =
            serde_json::from_str(&fit_json).expect("the stored breakdown parses");
        assert_eq!(
            breakdown.version, spec.version,
            "the stored rows carry the version that scored them"
        );
        assert!(
            (breakdown.total - expected.total).abs() < 1e-9,
            "the stored breakdown's total is the stored score"
        );
    }

    #[test]
    fn rescore_reports_the_rows_that_moved() {
        use chrono::Utc;
        use yagna_core::fit::default_spec;
        let scratch = Scratch::new("rescore-moved");
        let mut store = Store::open(&scratch.path()).expect("open");
        let now = Utc::now();
        store
            .upsert_snapshots(&[testing::sample_snapshot("owner", "repo")], now)
            .expect("upsert");
        let spec = default_spec();
        let first = store.rescore_fit(&spec, now).expect("rescore");
        assert_eq!(first.scored, 1);
        assert_eq!(first.moved, 1, "a first score moves the row from nothing");
        let second = store.rescore_fit(&spec, now).expect("rescore");
        assert_eq!(second.moved, 0, "the same spec moves nothing");
    }

    #[test]
    fn rescore_fit_row_scores_one_repository_alone() {
        use chrono::Utc;
        use yagna_core::fit::default_spec;
        let scratch = Scratch::new("rescore-row");
        let mut store = Store::open(&scratch.path()).expect("open");
        let now = Utc::now();
        store
            .upsert_snapshots(
                &[
                    testing::sample_snapshot("owner", "one"),
                    testing::sample_snapshot("owner", "two"),
                ],
                now,
            )
            .expect("upsert");
        let one = store
            .repo_rowid("github", "owner", "one")
            .expect("lookup")
            .expect("row");
        let moved = store
            .rescore_fit_row(one, &default_spec(), now)
            .expect("row");
        assert_eq!(moved, 1);
        assert!(store.fit_score_of(one).expect("read").is_some());
        let two = store
            .repo_rowid("github", "owner", "two")
            .expect("lookup")
            .expect("row");
        assert!(
            store.fit_score_of(two).expect("read").is_none(),
            "only the one row was scored"
        );
    }

    #[test]
    fn starred_at_of_reads_the_readers_star_time() {
        use chrono::Utc;
        let scratch = Scratch::new("starred-at");
        let mut store = Store::open(&scratch.path()).expect("open");
        let now = Utc::now();
        let snapshot = testing::sample_snapshot("owner", "repo");
        let starred = snapshot.starred_at.expect("fixture carries a star");
        store.upsert_snapshots(&[snapshot], now).expect("upsert");
        let rowid = store
            .repo_rowid("github", "owner", "repo")
            .expect("lookup")
            .expect("row");
        assert_eq!(store.starred_at_of(rowid).expect("read"), Some(starred));
        assert_eq!(store.starred_at_of(999_999).expect("read"), None);
    }

    #[test]
    fn the_fit_spread_buckets_rows_by_fraction_of_their_maximum() {
        use chrono::Utc;
        use yagna_core::fit::{FitSpec, parse_spec};
        let scratch = Scratch::new("fit-spread");
        let mut store = Store::open(&scratch.path()).expect("open");
        let now = Utc::now();
        let stars = |count: u64| {
            let mut snapshot = testing::sample_snapshot("owner", &format!("repo-{count}"));
            snapshot.stars = count;
            snapshot
        };
        store
            .upsert_snapshots(
                &[stars(0), stars(5), stars(10), stars(100), stars(777)],
                now,
            )
            .expect("upsert");
        let spec: FitSpec = parse_spec(
            r#"{"version":1,"name":"t","parameters":[{"name":"stars","weight":1,"bands":[
                {"max":0,"points":0},{"max":5,"points":0.25},{"max":10,"points":0.5},{"points":1}]}]}"#,
        )
        .expect("spec");
        for name in ["repo-0", "repo-5", "repo-10", "repo-100"] {
            let rowid = store
                .repo_rowid("github", "owner", name)
                .expect("lookup")
                .expect("row");
            store.rescore_fit_row(rowid, &spec, now).expect("row");
        }
        let spread = store.stats_fit_buckets().expect("spread");
        assert_eq!(spread.strong, 1, "the 100-star row fills its maximum");
        assert_eq!(spread.solid, 1);
        assert_eq!(spread.weak, 1);
        assert_eq!(spread.poor, 1, "the zero-star row");
        assert_eq!(spread.unscored, 1, "the 777-star row was never scored");
    }

    /// The notes migration repurposes the categories: every repository
    /// that carried one or more becomes the bearer of a note that keeps
    /// their names, the category tags themselves go away, and the plain
    /// tags are untouched. A database seeded at version 10 with
    /// categories attached is migrated by the next open.
    #[test]
    fn the_categories_become_notes_on_the_repositories_that_carried_them() {
        let scratch = Scratch::new("categories-to-notes");
        let path = scratch.path();
        seed_version_ten_with_categories(&path);

        let store = Store::open(&path).expect("migrated open");
        assert_eq!(store.schema_version().expect("version"), 13);

        let tags = store.tags_with_counts(None).expect("tags");
        assert!(
            tags.iter().all(|tag| tag.kind == TagKind::Tag),
            "no category rows survive: {tags:?}"
        );
        assert_eq!(tags.len(), 1, "the plain tag stands");
        assert_eq!(tags[0].name, "rust");

        let note = store
            .note_of(yagna_core::model::NoteSubject::Repo(1))
            .expect("read");
        let note = note.expect("the first repository carries the note");
        assert!(
            note.starts_with("unfiled: "),
            "the note names where it came from: {note}"
        );
        assert!(
            note.contains("books"),
            "the category's name survives: {note}"
        );
        assert!(note.contains("parser"), "both categories survive: {note}");
        assert_eq!(
            store
                .note_of(yagna_core::model::NoteSubject::Repo(2))
                .expect("read"),
            None,
            "the repository without categories carries no note"
        );
    }

    /// Builds a version-10 database with two repositories, a plain tag
    /// and two categories attached to the first repository — the shape
    /// the notes migration finds and converts.
    fn seed_version_ten_with_categories(path: &std::path::Path) {
        let conn = Connection::open(path).expect("raw open");
        for index in 0..10u32 {
            apply_migration(&conn, index).expect("seed migration {index}");
        }
        let seed_at = "2026-10-29T12:00:00Z";
        conn.execute(
            "INSERT INTO repos (forge, repo_id, owner, name, stars, created_at, starred_at,
                                last_commit_at, first_commit_date, first_commit_source,
                                commit_count, is_fork, is_archived, first_seen_at, last_synced_at)
             VALUES ('github', 1, 'someone', 'first', 10, ?1, ?1, ?1, ?1, 'repo-created',
                     0, 0, 0, ?1, ?1),
                    ('github', 2, 'someone', 'second', 10, ?1, ?1, ?1, ?1, 'repo-created',
                     0, 0, 0, ?1, ?1)",
            [seed_at],
        )
        .expect("seed repos");
        conn.execute(
            "INSERT INTO tags (name, kind) VALUES ('rust', 'tag'), ('books', 'category'),
                                              ('parser', 'category')",
            [],
        )
        .expect("seed tags");
        conn.execute(
            "INSERT INTO repo_tags (repo_rowid, tag_id)
             SELECT 1, id FROM tags WHERE name IN ('rust', 'books', 'parser')",
            [],
        )
        .expect("seed attachments");
    }

    #[test]
    fn open_refuses_a_database_newer_than_the_known_migrations() {
        let scratch = Scratch::new("too-new");
        let path = scratch.path();
        let raw = Connection::open(&path).expect("raw open");
        raw.pragma_update(None, "user_version", 99i64)
            .expect("stamp");
        drop(raw);
        let error = Store::open(&path).expect_err("refused");
        assert!(matches!(error, StoreError::SchemaTooNew { .. }));
    }

    /// The Part01 pitfall, cashed: full-text search is only real if the
    /// bundled SQLite can create an FTS5 table. If this fails, no
    /// FTS-using code may land until the store switches engine.
    #[test]
    fn invariant_fts5_virtual_tables_create_cleanly() {
        let scratch = Scratch::new("fts5");
        let store = Store::open(&scratch.path()).expect("open");
        store
            .conn
            .execute_batch("CREATE VIRTUAL TABLE temp.fts_probe USING fts5(body)")
            .expect("FTS5 is missing from the bundled SQLite");
        store
            .conn
            .execute(
                "INSERT INTO temp.fts_probe (body) VALUES (?1)",
                ["readme search text"],
            )
            .expect("fts5 insert");
        let hits = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM temp.fts_probe WHERE body MATCH ?1",
                ["readme"],
                |row| row.get::<_, i64>(0),
            )
            .expect("fts5 match");
        assert_eq!(hits, 1);
    }
}
