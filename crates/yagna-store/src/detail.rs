//! The repository tab's read side: the detail record the tab's metadata
//! block shows, the stored bodies the tab renders (the README, the tree,
//! the license text), the assets the README points at, and the writes
//! the tab asks for (the exact first commit, the queue rows the tab's
//! own actions enqueue — including the README the row and the tab ask
//! for on demand).
//!
//! Everything here is one query per verb — the tab reads through the
//! store facade, never the database, and never per-field follow-ups.
//!
//! Items: `RepoDetail` (the core's detail record), `ReadmeBody` — the
//! stored README's rewritten body; `TreeBody` — the stored tree's
//! rendered text; `DetailStore` — the verbs themselves.
//! Depends on: rusqlite, chrono, yagna_core (the detail record, the
//! health report), crate::catalog (the timestamp format and the row id
//! helpers).
//! Called by: the app's repository tab.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;
use yagna_core::health::HealthReport;
use yagna_core::model::{Forge, LanguageStat, LicenseInfo, ReleaseInfo, RepoDetail, RepoRef};

use crate::{QueueKind, Store, StoreError, assets, catalog, saturating_rowid};

/// The stored README's body: the rewritten text (image destinations are
/// asset ids) and whether the store cut it at its cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmeBody {
    pub rewritten: String,
    pub truncated: bool,
}

/// The stored tree's body: the rendered text and the forge's own
/// truncation flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBody {
    pub rendered: String,
    pub truncated: bool,
}

/// The first-commit source the exact enrichment writes: the date came
/// out of the repository's git history.
pub const FIRST_COMMIT_SOURCE_GIT_HISTORY: &str = "git-history";

/// What a pre-fetch covers: the repositories it reaches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefetchScope {
    /// Every repository in the catalog.
    All,
    /// The repositories whose primary language is this one — the "I
    /// only work with Rust this month" case.
    Language(String),
    /// The repositories carrying every one of these tag names.
    Tags(Vec<String>),
    /// The repositories carrying every one of these tags, by id — the
    /// filter's own selection.
    TagIds(Vec<i64>),
    /// One repository, by owner and name.
    Repository(String, String),
}

/// What a pre-fetch pulls of each repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefetchPlan {
    /// The images: none, the first bytes of each repository's images, or
    /// all of them.
    pub images: ImagePolicy,
    /// The licence texts.
    pub licenses: bool,
    /// The folder structures.
    pub trees: bool,
}

impl PrefetchPlan {
    /// The plan that pulls the bodies only: the READMEs, no images, no
    /// licences, no trees.
    pub fn bodies_only() -> Self {
        Self {
            images: ImagePolicy::None,
            licenses: false,
            trees: false,
        }
    }
}

/// How many of a repository's images a pre-fetch pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePolicy {
    /// No images at all.
    None,
    /// The images, until the repository's cached images have taken this
    /// many bytes.
    FirstBytes(u64),
    /// Every image of the repository.
    All,
}

impl Store {
    /// One repository's detail record: every field the tab's metadata
    /// block shows, in one read. `None` when the row is not there (or
    /// has gone missing).
    pub fn repo_detail(&self, repo_rowid: u64) -> Result<Option<RepoDetail>, StoreError> {
        let mut statement = self.conn.prepare(DETAIL_SELECT)?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(map_detail(row)?)),
        }
    }

    /// The stored README's rewritten body — the text the renderer reads.
    /// `None` when no README was fetched for this repository.
    pub fn readme_body(&self, repo_rowid: u64) -> Result<Option<ReadmeBody>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT rewritten, truncated FROM readmes WHERE repo_rowid = ?1")?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(ReadmeBody {
                rewritten: row.get(0)?,
                truncated: row.get::<_, i64>(1)? != 0,
            })),
        }
    }

    /// The stored tree's rendered text — the shape the tab's tree widget
    /// draws. `None` when no tree was fetched.
    pub fn tree_body(&self, repo_rowid: u64) -> Result<Option<TreeBody>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT rendered, truncated FROM trees WHERE repo_rowid = ?1")?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(TreeBody {
                rendered: row.get(0)?,
                truncated: row.get::<_, i64>(1)? != 0,
            })),
        }
    }

    /// The stored license text — the non-standard licenses only, the
    /// ones the reader must read before using. A read error surfaces,
    /// never reads as "no license": a busy database is not an answer.
    pub fn license_body(&self, repo_rowid: u64) -> Result<Option<String>, StoreError> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM license_texts WHERE repo_rowid = ?1",
                params![saturating_rowid(repo_rowid)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(body)
    }

    /// Every asset the repository's README points at, in the order the
    /// README mentions them. The resolver asks for one id at a time; the
    /// tab keeps this list as its answer book.
    pub fn repo_assets(&self, repo_rowid: u64) -> Result<Vec<assets::AssetRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT a.asset_id, a.url, a.badge, a.state, a.path, a.bytes
               FROM readme_assets ra JOIN assets a ON a.asset_id = ra.asset_id
              WHERE ra.repo_rowid = ?1
              ORDER BY ra.position ASC",
        )?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        let mut found = Vec::new();
        while let Some(row) = rows.next()? {
            found.push(assets::map_asset(row)?);
        }
        Ok(found)
    }

    /// How many bytes the repository's cached images take — the input to
    /// the per-repository byte budget.
    pub fn repo_asset_bytes(&self, repo_rowid: u64) -> Result<u64, StoreError> {
        let bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(a.bytes), 0)
               FROM readme_assets ra JOIN assets a ON a.asset_id = ra.asset_id
              WHERE ra.repo_rowid = ?1 AND a.state = 'present'",
            params![saturating_rowid(repo_rowid)],
            |row| row.get(0),
        )?;
        Ok(bytes.max(0) as u64)
    }

    /// Enqueues every missing non-badge image of one repository — the
    /// tab's first open does this, and the runner works the queue.
    /// Badges are never downloaded; a live (pending-or-claimed) queue
    /// row is never duplicated.
    pub fn enqueue_repo_assets(
        &mut self,
        repo_rowid: u64,
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO asset_queue (asset_id, state, updated_at)
             SELECT a.asset_id, 'pending', ?2
               FROM readme_assets ra JOIN assets a ON a.asset_id = ra.asset_id
              WHERE ra.repo_rowid = ?1 AND a.badge = 0 AND a.state != 'present'
                AND NOT EXISTS (
                  SELECT 1 FROM asset_queue q
                   WHERE q.asset_id = a.asset_id AND q.state IN ('pending', 'claimed', 'done')
                )",
            params![saturating_rowid(repo_rowid), catalog::stamp(now)],
        )?;
        Ok(inserted as u64)
    }

    /// Claims the oldest eligible image of one repository under a
    /// lease, like every other claim: the app's runner works one
    /// repository's images at a time, a lapsed lease re-queues, and the
    /// claim names itself with an ownership token.
    pub fn claim_repo_asset(
        &self,
        repo_rowid: u64,
    ) -> Result<Option<assets::AssetJob>, StoreError> {
        let now = Utc::now();
        let transaction = crate::ImmediateTx::begin(&self.conn)?;
        let id: Option<i64> = transaction
            .conn
            .query_row(
                "SELECT q.id FROM asset_queue q
                   JOIN readme_assets ra ON ra.asset_id = q.asset_id
                  WHERE ra.repo_rowid = ?1
                    AND (q.state = 'pending'
                         OR (q.state = 'claimed'
                             AND (q.lease_until IS NULL OR q.lease_until <= ?2)))
                    AND (q.not_before IS NULL OR q.not_before <= ?2)
                  ORDER BY q.id LIMIT 1",
                params![saturating_rowid(repo_rowid), catalog::stamp(now)],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        transaction.conn.execute(
            "UPDATE asset_queue
             SET state = 'claimed', lease_until = ?2, updated_at = ?3,
                 claim_token = lower(hex(randomblob(16)))
              WHERE id = ?1",
            params![
                id,
                catalog::stamp(now + crate::enrichment::lease_duration()),
                catalog::stamp(now),
            ],
        )?;
        let job = assets::read_job(transaction.conn, id)?;
        transaction.commit()?;
        Ok(Some(job))
    }

    /// Which of the kinds the repository still lacks — what a pre-fetch
    /// pulls.
    pub fn missing_kinds(&self, repo_rowid: u64) -> Result<Vec<QueueKind>, StoreError> {
        let rowid = saturating_rowid(repo_rowid);
        let mut missing = Vec::new();
        for (kind, table) in [
            (QueueKind::Readme, "readmes"),
            (QueueKind::License, "license_texts"),
            (QueueKind::Tree, "trees"),
        ] {
            let count: i64 = self.conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE repo_rowid = ?1"),
                params![rowid],
                |row| row.get(0),
            )?;
            if count == 0 {
                missing.push(kind);
            }
        }
        Ok(missing)
    }

    /// The repositories a scope reaches: their row ids, in the
    /// catalog's own order.
    pub fn scope_ids(&self, scope: &PrefetchScope) -> Result<Vec<i64>, StoreError> {
        let sql = match scope {
            PrefetchScope::All => {
                "SELECT id FROM repos WHERE missing_at IS NULL ORDER BY id".to_string()
            }
            PrefetchScope::Language(_) => {
                "SELECT id FROM repos WHERE missing_at IS NULL AND lower(primary_language) = lower(?1)
                  ORDER BY id"
                    .to_string()
            }
            PrefetchScope::Tags(names) => {
                if names.is_empty() {
                    return Ok(Vec::new());
                }
                let placeholders = (1..=names.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "SELECT r.id FROM repos r
                       JOIN repo_tags rt ON rt.repo_rowid = r.id
                       JOIN tags t ON t.id = rt.tag_id
                      WHERE r.missing_at IS NULL AND lower(t.name) IN ({placeholders})
                      GROUP BY r.id
                     HAVING COUNT(DISTINCT lower(t.name)) = {}
                      ORDER BY r.id",
                    names.len()
                )
            }
            PrefetchScope::Repository(_, _) => {
                "SELECT id FROM repos WHERE missing_at IS NULL AND lower(owner) = lower(?1)
                   AND lower(name) = lower(?2) ORDER BY id"
                    .to_string()
            }
            PrefetchScope::TagIds(ids) => {
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let placeholders = (1..=ids.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "SELECT r.id FROM repos r
                       JOIN repo_tags rt ON rt.repo_rowid = r.id
                      WHERE r.missing_at IS NULL AND rt.tag_id IN ({placeholders})
                      GROUP BY r.id
                     HAVING COUNT(DISTINCT rt.tag_id) = {}
                      ORDER BY r.id",
                    ids.len()
                )
            }
        };
        let values: Vec<Box<dyn rusqlite::ToSql>> = match scope {
            PrefetchScope::All => Vec::new(),
            PrefetchScope::Language(language) => vec![Box::new(language.clone())],
            PrefetchScope::Tags(names) => names
                .iter()
                .map(|name| Box::new(name.to_lowercase()) as Box<dyn rusqlite::ToSql>)
                .collect(),
            PrefetchScope::Repository(owner, name) => {
                vec![Box::new(owner.clone()), Box::new(name.clone())]
            }
            PrefetchScope::TagIds(ids) => ids
                .iter()
                .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
                .collect(),
        };
        let mut statement = self.conn.prepare(&sql)?;
        let ids = statement.query_map(rusqlite::params_from_iter(values.iter()), |row| {
            row.get::<_, i64>(0)
        })?;
        let mut found = Vec::new();
        for id in ids {
            found.push(id?);
        }
        Ok(found)
    }

    /// Enqueues the README of one repository — the on-demand path the
    /// expanded row and the repository tab share. A repository that
    /// already has a README, or a live queue row, is not enqueued
    /// again: one fetch per repository, never a burst.
    pub fn enqueue_readme(
        &mut self,
        repo_rowid: u64,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
             SELECT ?1, 'readme', ?2
              WHERE NOT EXISTS (SELECT 1 FROM readmes WHERE readmes.repo_rowid = ?1)
                AND NOT EXISTS (
                  SELECT 1 FROM enrichment_queue
                   WHERE enrichment_queue.repo_rowid = ?1
                     AND enrichment_queue.kind = 'readme'
                     AND enrichment_queue.state IN ('pending', 'claimed', 'done', 'skipped')
                )",
            params![saturating_rowid(repo_rowid), catalog::stamp(now)],
        )?;
        Ok(inserted > 0)
    }

    /// Enqueues the licence text of one repository — the tab's
    /// "fetch the license text" action. Only a repository whose swept
    /// SPDX id is missing or says NOASSERTION has a licence worth
    /// fetching: a standard licence is a name, not a file.
    pub fn enqueue_license(
        &mut self,
        repo_rowid: u64,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
             SELECT ?1, 'license', ?2
              FROM repos
              WHERE id = ?1
                AND (license_spdx IS NULL OR license_spdx = 'NOASSERTION')
                AND NOT EXISTS (SELECT 1 FROM license_texts WHERE license_texts.repo_rowid = ?1)
                AND NOT EXISTS (
                  SELECT 1 FROM enrichment_queue
                   WHERE enrichment_queue.repo_rowid = ?1
                     AND enrichment_queue.kind = 'license'
                     AND enrichment_queue.state IN ('pending', 'claimed', 'done', 'skipped')
                )",
            params![saturating_rowid(repo_rowid), catalog::stamp(now)],
        )?;
        Ok(inserted > 0)
    }

    /// Enqueues the tree of one repository — the tab's "load structure"
    /// action. A repository that already has a tree, or a live queue
    /// row, is not enqueued again.
    pub fn enqueue_tree(
        &mut self,
        repo_rowid: u64,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
             SELECT ?1, 'tree', ?2
              WHERE NOT EXISTS (SELECT 1 FROM trees WHERE trees.repo_rowid = ?1)
                AND NOT EXISTS (
                  SELECT 1 FROM enrichment_queue
                   WHERE enrichment_queue.repo_rowid = ?1
                     AND enrichment_queue.kind = 'tree'
                     AND enrichment_queue.state IN ('pending', 'claimed', 'done', 'skipped')
                )",
            params![saturating_rowid(repo_rowid), catalog::stamp(now)],
        )?;
        Ok(inserted > 0)
    }

    /// Writes the exact first commit: the date and where it came from.
    /// The source is what makes the enrichment happen once — a second
    /// open sees `git-history` and asks no more.
    pub fn set_first_commit(
        &mut self,
        repo_rowid: u64,
        date: DateTime<Utc>,
        source: &str,
    ) -> Result<bool, StoreError> {
        let written = self.conn.execute(
            "UPDATE repos SET first_commit_date = ?2, first_commit_source = ?3 WHERE id = ?1",
            params![saturating_rowid(repo_rowid), catalog::stamp(date), source,],
        )?;
        Ok(written > 0)
    }
}

/// The detail query: every column the tab's metadata block shows.
const DETAIL_SELECT: &str = "
SELECT
  r.id, r.forge, r.owner, r.name, r.description, r.stars,
  r.languages_json, r.license_name, r.license_spdx,
  r.topics_json, r.default_branch,
  r.created_at, r.starred_at, r.last_commit_at,
  r.first_commit_date, r.first_commit_source,
    r.commit_count, r.latest_release_tag, r.latest_release_at,
    r.is_fork, r.is_archived, r.health_json, r.fit_json
  FROM repos r
  WHERE r.id = ?1
  ";

/// The column indexes of `DETAIL_SELECT`, kept beside the mapper so the
/// two cannot drift apart.
mod detail_col {
    pub(crate) const ROWID: usize = 0;
    pub(crate) const FORGE: usize = 1;
    pub(crate) const OWNER: usize = 2;
    pub(crate) const NAME: usize = 3;
    pub(crate) const DESCRIPTION: usize = 4;
    pub(crate) const STARS: usize = 5;
    pub(crate) const LANGUAGES_JSON: usize = 6;
    pub(crate) const LICENSE_NAME: usize = 7;
    pub(crate) const LICENSE_SPDX: usize = 8;
    pub(crate) const TOPICS_JSON: usize = 9;
    pub(crate) const DEFAULT_BRANCH: usize = 10;
    pub(crate) const CREATED_AT: usize = 11;
    pub(crate) const STARRED_AT: usize = 12;
    pub(crate) const LAST_COMMIT_AT: usize = 13;
    pub(crate) const FIRST_COMMIT_DATE: usize = 14;
    pub(crate) const FIRST_COMMIT_SOURCE: usize = 15;
    pub(crate) const COMMIT_COUNT: usize = 16;
    pub(crate) const LATEST_RELEASE_TAG: usize = 17;
    pub(crate) const LATEST_RELEASE_AT: usize = 18;
    pub(crate) const IS_FORK: usize = 19;
    pub(crate) const IS_ARCHIVED: usize = 20;
    pub(crate) const HEALTH_JSON: usize = 21;
    pub(crate) const FIT_JSON: usize = 22;
}

/// Maps one detail row into the core's detail record.
fn map_detail(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepoDetail> {
    map_detail_inner(row).map_err(store_error_to_rusqlite)
}

/// The mapping itself: every parse failure is a store error — the
/// columns were written by this store.
fn map_detail_inner(row: &rusqlite::Row<'_>) -> Result<RepoDetail, StoreError> {
    let forge_text: String = row.get(detail_col::FORGE)?;
    let forge = match forge_text.as_str() {
        "github" => Forge::GitHub,
        other => return Err(unreadable(&format!("forge {other:?}"))),
    };
    let languages: Vec<LanguageStat> =
        match row.get::<_, Option<String>>(detail_col::LANGUAGES_JSON)? {
            Some(text) => serde_json::from_str(&text)?,
            None => Vec::new(),
        };
    let topics: Vec<String> = match row.get::<_, Option<String>>(detail_col::TOPICS_JSON)? {
        Some(text) => serde_json::from_str(&text)?,
        None => Vec::new(),
    };
    let health: HealthReport = match row.get::<_, Option<String>>(detail_col::HEALTH_JSON)? {
        Some(text) => serde_json::from_str(&text)?,
        None => HealthReport::default(),
    };
    let fit: Option<yagna_core::fit::FitReport> =
        match row.get::<_, Option<String>>(detail_col::FIT_JSON)? {
            Some(text) => Some(serde_json::from_str(&text)?),
            None => None,
        };
    let license = match row.get::<_, Option<String>>(detail_col::LICENSE_NAME)? {
        Some(name) => Some(LicenseInfo {
            name,
            spdx_id: row.get(detail_col::LICENSE_SPDX)?,
        }),
        None => None,
    };
    let latest_release = match row.get::<_, Option<String>>(detail_col::LATEST_RELEASE_TAG)? {
        Some(tag) => Some(ReleaseInfo {
            tag,
            published_at: match row.get::<_, Option<String>>(detail_col::LATEST_RELEASE_AT)? {
                Some(text) => Some(catalog::parse_stamp(&text)?),
                None => None,
            },
        }),
        None => None,
    };
    Ok(RepoDetail {
        repo_rowid: row.get(detail_col::ROWID)?,
        repo_ref: RepoRef {
            forge,
            owner: row.get(detail_col::OWNER)?,
            name: row.get(detail_col::NAME)?,
        },
        description: row.get(detail_col::DESCRIPTION)?,
        stars: row.get::<_, i64>(detail_col::STARS)?.max(0) as u64,
        languages,
        license,
        topics,
        default_branch: row.get(detail_col::DEFAULT_BRANCH)?,
        created_at: stamp_or_epoch(row, detail_col::CREATED_AT)?,
        starred_at: match row.get::<_, Option<String>>(detail_col::STARRED_AT)? {
            Some(text) => Some(catalog::parse_stamp(&text)?),
            None => None,
        },
        last_commit_at: stamp_or_epoch(row, detail_col::LAST_COMMIT_AT)?,
        first_commit_date: match row.get::<_, Option<String>>(detail_col::FIRST_COMMIT_DATE)? {
            Some(text) => Some(catalog::parse_stamp(&text)?),
            None => None,
        },
        first_commit_source: row.get(detail_col::FIRST_COMMIT_SOURCE)?,
        commit_count: row.get::<_, i64>(detail_col::COMMIT_COUNT)?.max(0) as u64,
        latest_release,
        is_fork: row.get::<_, i64>(detail_col::IS_FORK)? != 0,
        is_archived: row.get::<_, i64>(detail_col::IS_ARCHIVED)? != 0,
        health,
        fit,
    })
}

/// A date column back into an instant; an absent one is the epoch (the
/// column is nullable in the schema, and the sweep always fills it).
fn stamp_or_epoch(row: &rusqlite::Row<'_>, column: usize) -> Result<DateTime<Utc>, StoreError> {
    match row.get::<_, Option<String>>(column)? {
        Some(text) => catalog::parse_stamp(&text),
        None => Ok(DateTime::<Utc>::UNIX_EPOCH),
    }
}

/// Carries a mapping failure out of `query_map`: the store's own errors
/// pass through; anything else becomes the generic conversion failure.
fn store_error_to_rusqlite(error: StoreError) -> rusqlite::Error {
    match error {
        StoreError::Sqlite(inner) => inner,
        other => rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(other),
        ),
    }
}

/// A `StoreError::Parse` for a stored value that does not read back.
fn unreadable(what: &str) -> StoreError {
    StoreError::Parse(format!("cannot read {what} from the catalog"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    /// The row id of the seeded repository.
    fn seed(store: &mut Store) -> u64 {
        let snapshots = vec![crate::testing::sample_snapshot("someone", "something")];
        store.upsert_snapshots(&snapshots, now()).expect("seed");
        let id: i64 = store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get(0))
            .expect("rowid");
        id as u64
    }

    #[test]
    fn the_detail_carries_every_display_field() {
        let (_scratch, mut store) = store("detail");
        let id = seed(&mut store);
        let detail = store
            .repo_detail(id)
            .expect("detail")
            .expect("a detail record");
        assert_eq!(detail.repo_ref.owner, "someone");
        assert_eq!(detail.repo_ref.name, "something");
        assert_eq!(detail.stars, 10);
        assert_eq!(detail.commit_count, 500);
        assert_eq!(detail.default_branch.as_deref(), Some("main"));
        assert_eq!(detail.first_commit_source.as_deref(), Some("repo-created"));
        assert_eq!(
            detail.license.as_ref().map(|l| l.spdx_id.clone()),
            Some(Some("MIT".to_string()))
        );
        assert!(detail.created_at < detail.last_commit_at);
        assert!(store.repo_detail(9999).expect("detail").is_none());
    }

    #[test]
    fn the_bodies_read_back_and_absent_ones_are_none() {
        let (_scratch, mut store) = store("bodies");
        let id = seed(&mut store);
        assert!(store.readme_body(id).expect("readme").is_none());
        assert!(store.tree_body(id).expect("tree").is_none());
        assert!(store.license_body(id).expect("license").is_none());

        store
            .store_readme(id, "# Demo\n\n![shot](docs/shot.png)\n", now())
            .expect("readme");
        let readme = store.readme_body(id).expect("readme").expect("a body");
        assert!(readme.rewritten.contains("yagna-asset://"));
        assert!(!readme.truncated);

        store
            .store_tree(
                id,
                &[yagna_core::model::TreeEntry {
                    path: "src".to_string(),
                    kind: yagna_core::model::TreeEntryKind::Tree,
                    size: None,
                }],
                false,
                now(),
            )
            .expect("tree");
        let tree = store.tree_body(id).expect("tree").expect("a body");
        assert!(tree.rendered.contains("src"));

        store
            .store_license_text(id, "Hippocratic License 2.1", now())
            .expect("license");
        assert_eq!(
            store.license_body(id).expect("license").as_deref(),
            Some("Hippocratic License 2.1")
        );
    }

    #[test]
    fn the_repository_assets_are_the_ones_the_readme_points_at() {
        let (_scratch, mut store) = store("assets");
        let id = seed(&mut store);
        let md = "# Demo\n\n![badge](https://img.shields.io/badge/x-y-green)\n\n![shot](docs/shot.png)\n";
        store.store_readme(id, md, now()).expect("readme");

        let assets = store.repo_assets(id).expect("assets");
        assert_eq!(assets.len(), 2, "the badge and the screenshot");
        assert_eq!(store.repo_asset_bytes(id).expect("bytes"), 0);

        let queued = store.enqueue_repo_assets(id, now()).expect("queue");
        assert_eq!(queued, 1, "the badge is never downloaded");
        assert_eq!(
            store.enqueue_repo_assets(id, now()).expect("queue"),
            0,
            "the fill is idempotent"
        );

        let job = store.claim_repo_asset(id).expect("claim").expect("a job");
        assert!(!job.asset_id.is_empty());
        store
            .stamp_asset_done(job.id, &job.claim_token, now())
            .expect("stamp");
        assert!(
            store.claim_repo_asset(id).expect("claim").is_none(),
            "the repository's queue is drained"
        );
    }

    #[test]
    fn the_tree_enqueue_is_idempotent_and_claims_one_repository() {
        let (_scratch, mut store) = store("tree-enqueue");
        let id = seed(&mut store);
        assert!(store.enqueue_tree(id, now()).expect("enqueue"));
        assert!(
            !store.enqueue_tree(id, now()).expect("enqueue"),
            "the live row is not duplicated"
        );
        let rows: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM enrichment_queue WHERE kind = 'tree'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(rows, 1);
    }

    /// The README enqueue is idempotent while the row is live, and a
    /// stored README ends the need for it — one fetch per repository.
    #[test]
    fn the_readme_enqueue_is_idempotent_and_a_stored_body_ends_it() {
        let (_scratch, mut store) = store("readme-enqueue");
        let id = seed(&mut store);
        assert!(store.enqueue_readme(id, now()).expect("enqueue"));
        assert!(
            !store.enqueue_readme(id, now()).expect("enqueue"),
            "the live row is not duplicated"
        );

        let item = store
            .claim_pending_for(id, QueueKind::Readme)
            .expect("claim")
            .expect("an item");
        store
            .stamp_done(item.id, &item.claim_token, now())
            .expect("done");
        store.store_readme(id, "# Demo\n", now()).expect("store");
        assert!(
            !store.enqueue_readme(id, now()).expect("enqueue"),
            "a satisfied repository is not enqueued again"
        );
    }

    #[test]
    fn the_first_commit_write_flips_the_source() {
        let (_scratch, mut store) = store("detail-first-commit");
        let id = seed(&mut store);
        assert!(
            store
                .set_first_commit(id, now(), FIRST_COMMIT_SOURCE_GIT_HISTORY)
                .expect("write")
        );
        let detail = store.repo_detail(id).expect("detail").expect("a record");
        assert_eq!(
            detail.first_commit_source.as_deref(),
            Some(FIRST_COMMIT_SOURCE_GIT_HISTORY)
        );
        assert_eq!(detail.first_commit_date, Some(now()));
    }
}
