//! The export and import side: the catalog's rows read out in the
//! export shape, and the export shape written back in.
//!
//! The export reads every repository row in one query — the snapshot
//! fields, the stored health, and the user's tags folded in as one
//! JSON array — and the import merges them back by the identity key
//! `(forge, owner, name)`: a repository that is new is inserted, one
//! that is known is updated in place, and the user's tags are merged
//! (never clobbered — an existing tag keeps its kind, an existing
//! attachment is left alone). The import is idempotent: the same file
//! twice changes nothing the second time, because the write only
//! happens when a field actually differs.
//!
//! Items: `export_repos` — the catalog in the export shape;
//! `import_repos` — the merge; `ImportReport` — what the merge did;
//! `backup_to` — the SQL format's consistent copy of the database.
//! Depends on: rusqlite (the rows and the backup API), serde_json (the
//! JSON columns), chrono (instants), yagna_core::export (the shape),
//! yagna_core::health (the score), crate::catalog (the stamp format).
//! Called by: the app crate's export and import verbs.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;
use yagna_core::export::{ExportExtension, ExportRepo, ExportTag};
use yagna_core::health::HealthReport;
use yagna_core::model::{Forge, LanguageStat, LicenseInfo, ReleaseInfo, RepoSnapshot};

use crate::{Store, StoreError, catalog};

/// What one import did: the rows that were new, the rows that changed,
/// the rows that named a forge this build does not know (skipped, not
/// guessed at), and the tag attachments that were added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub skipped: u64,
    pub tagged: u64,
}

/// The export's read: every repository row, in the catalog's order.
/// The id is the service's own repository id (the exporter's `id`),
/// not the catalog's rowid — the rowid is the library's bookkeeping.
const EXPORT_SELECT: &str = "
SELECT r.repo_id, r.forge, r.owner, r.name, r.description, r.stars, r.primary_language,
       r.topics_json, r.created_at, r.last_commit_at, r.starred_at, r.commit_count,
       r.languages_json, r.license_name, r.license_spdx, r.default_branch,
       r.latest_release_tag, r.latest_release_at, r.is_fork, r.is_archived,
       r.first_commit_date, r.first_commit_source, r.health_json, r.missing_at,
       (SELECT json_group_array(json_object('name', t.name, 'kind', t.kind))
          FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
         WHERE rt.repo_rowid = r.id) AS tags_json
  FROM repos r
 ORDER BY r.id";

impl Store {
    /// The catalog in the export shape: every repository, with its
    /// tags, its stored health and its enrichment facts.
    pub fn export_repos(&self) -> Result<Vec<ExportRepo>, StoreError> {
        let mut statement = self.conn.prepare(EXPORT_SELECT)?;
        let rows = statement.query_map([], map_export_repo)?;
        let mut repos = Vec::new();
        for row in rows {
            repos.push(row?);
        }
        Ok(repos)
    }

    /// Merges exported repositories into the catalog. A new identity is
    /// inserted; a known one is updated — but only where a field
    /// actually differs, so the same file imported twice changes
    /// nothing the second time. The user's tags are merged: a new tag
    /// is created, an existing one keeps its kind, and an existing
    /// attachment is left alone.
    pub fn import_repos(
        &mut self,
        repos: &[ExportRepo],
        now: DateTime<Utc>,
    ) -> Result<ImportReport, StoreError> {
        // Immediate, like the sweep's upsert: the merge reads before it
        // writes.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut report = ImportReport::default();
        for repo in repos {
            let Some(snapshot) = snapshot_of(repo) else {
                report.skipped += 1;
                continue;
            };
            let known: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE forge = ?1 AND owner = ?2 AND name = ?3)",
                params![snapshot.forge.as_str(), snapshot.owner, snapshot.name],
                |row| row.get(0),
            )?;
            // The digest's staleness is decided before the row moves,
            // exactly as the sweep decides it: an import whose commit
            // moved must not leave a stale digest reading as fresh.
            let incoming_commit = catalog::stamp(snapshot.last_commit_at);
            let previous = crate::digests::stored_last_commit(
                &transaction,
                snapshot.forge.as_str(),
                &snapshot.owner,
                &snapshot.name,
            );
            let changed = upsert_imported(&transaction, repo, &snapshot, now)?;
            if let Some((rowid, stored_commit)) = previous
                && stored_commit != incoming_commit
            {
                crate::digests::stamp_stale(&transaction, rowid, &catalog::stamp(now));
            }
            if changed {
                if known {
                    report.updated += 1;
                } else {
                    report.inserted += 1;
                }
            } else {
                report.unchanged += 1;
            }
            report.tagged += merge_tags(&transaction, repo)?;
        }
        transaction.commit()?;
        Ok(report)
    }

    /// Writes a consistent copy of the database to `path` — the SQL
    /// export format. The backup API copies page by page while the
    /// library may be open elsewhere, so the copy is never a torn
    /// file.
    pub fn backup_to(&self, path: &Path) -> Result<(), StoreError> {
        let mut destination = rusqlite::Connection::open(path)?;
        let backup = rusqlite::backup::Backup::new(&self.conn, &mut destination)?;
        backup.run_to_completion(64, Duration::from_millis(25), None)?;
        Ok(())
    }
}

/// One exported repository's write: the row is inserted or updated,
/// and the write reports whether it changed anything.
fn upsert_imported(
    transaction: &rusqlite::Transaction<'_>,
    repo: &ExportRepo,
    snapshot: &RepoSnapshot,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let health = match &repo.yagna.health {
        Some(report) => *report,
        None => yagna_core::health::health(snapshot, now),
    };
    let first_commit = repo.yagna.first_commit_at.map(catalog::stamp);
    // The column is NOT NULL with a default the INSERT does not earn:
    // a foreign file without the fact still writes the honest default.
    let first_source = repo
        .yagna
        .first_commit_source
        .clone()
        .unwrap_or_else(|| catalog::FIRST_COMMIT_SOURCE_DEFAULT.to_string());
    // A carried "gone since" stamp is history and travels; only a file
    // that says missing without a stamp (a foreign writer) falls back
    // to now.
    let missing_at = repo
        .yagna
        .missing_at
        .clone()
        .or_else(|| repo.yagna.missing.then(|| catalog::stamp(now)));
    transaction.execute(
        IMPORT_UPSERT,
        params![
            snapshot.forge.as_str(),
            crate::saturating_rowid(snapshot.repo_id),
            snapshot.owner,
            snapshot.name,
            snapshot.description,
            crate::saturating_rowid(snapshot.stars),
            snapshot
                .languages
                .first()
                .map(|language| language.name.as_str()),
            serde_json::to_string(&snapshot.languages)?,
            snapshot
                .license
                .as_ref()
                .map(|license| license.name.as_str()),
            snapshot
                .license
                .as_ref()
                .and_then(|license| license.spdx_id.as_deref()),
            serde_json::to_string(&snapshot.topics)?,
            snapshot.default_branch.as_deref(),
            catalog::stamp(snapshot.created_at),
            snapshot.starred_at.map(catalog::stamp),
            catalog::stamp(snapshot.last_commit_at),
            first_commit,
            first_source,
            crate::saturating_rowid(snapshot.commit_count),
            snapshot
                .latest_release
                .as_ref()
                .map(|release| release.tag.as_str()),
            snapshot
                .latest_release
                .as_ref()
                .and_then(|release| release.published_at.map(catalog::stamp)),
            i64::from(snapshot.is_fork),
            i64::from(snapshot.is_archived),
            i64::from(health.total),
            serde_json::to_string(&health)?,
            catalog::stamp(now),
            catalog::stamp(now),
            missing_at,
        ],
    )?;
    Ok(transaction.changes() > 0)
}

/// The import's upsert: the identity key decides, and the update runs
/// only where a field actually differs — the second import of the same
/// file writes nothing.
const IMPORT_UPSERT: &str = "
INSERT INTO repos (
  forge, repo_id, owner, name, description, stars,
  primary_language, languages_json,
  license_name, license_spdx,
  topics_json, default_branch,
  created_at, starred_at, last_commit_at,
  first_commit_date, first_commit_source, commit_count,
  latest_release_tag, latest_release_at,
  is_fork, is_archived, health_score, health_json,
  first_seen_at, last_synced_at, missing_at
) VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
  ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
)
ON CONFLICT(forge, owner, name) DO UPDATE SET
  repo_id = excluded.repo_id,
  description = excluded.description,
  stars = excluded.stars,
  primary_language = excluded.primary_language,
  languages_json = excluded.languages_json,
  license_name = excluded.license_name,
  license_spdx = excluded.license_spdx,
  topics_json = excluded.topics_json,
  default_branch = excluded.default_branch,
  created_at = excluded.created_at,
  starred_at = excluded.starred_at,
  last_commit_at = excluded.last_commit_at,
  first_commit_date = excluded.first_commit_date,
  first_commit_source = excluded.first_commit_source,
  commit_count = excluded.commit_count,
  latest_release_tag = excluded.latest_release_tag,
  latest_release_at = excluded.latest_release_at,
  is_fork = excluded.is_fork,
  is_archived = excluded.is_archived,
  health_score = excluded.health_score,
  health_json = excluded.health_json,
  -- The stamp is history: an import without one (the repository is
  -- present again) keeps what the catalog already knows.
  missing_at = COALESCE(excluded.missing_at, repos.missing_at)
WHERE
  repos.repo_id IS NOT excluded.repo_id
  OR repos.description IS NOT excluded.description
  OR repos.stars IS NOT excluded.stars
  OR repos.primary_language IS NOT excluded.primary_language
  OR repos.languages_json IS NOT excluded.languages_json
  OR repos.license_name IS NOT excluded.license_name
  OR repos.license_spdx IS NOT excluded.license_spdx
  OR repos.topics_json IS NOT excluded.topics_json
  OR repos.default_branch IS NOT excluded.default_branch
  OR repos.created_at IS NOT excluded.created_at
  OR repos.starred_at IS NOT excluded.starred_at
  OR repos.last_commit_at IS NOT excluded.last_commit_at
  OR repos.first_commit_date IS NOT excluded.first_commit_date
  OR repos.first_commit_source IS NOT excluded.first_commit_source
  OR repos.commit_count IS NOT excluded.commit_count
  OR repos.latest_release_tag IS NOT excluded.latest_release_tag
  OR repos.latest_release_at IS NOT excluded.latest_release_at
  OR repos.is_fork IS NOT excluded.is_fork
  OR repos.is_archived IS NOT excluded.is_archived
  OR repos.health_score IS NOT excluded.health_score
  OR repos.health_json IS NOT excluded.health_json
  OR repos.missing_at IS NOT excluded.missing_at";

/// The user's tags, merged: a new tag is created with its kind, an
/// existing one keeps the kind it has, and the attachment is added
/// only once. Returns how many attachments were added.
fn merge_tags(
    transaction: &rusqlite::Transaction<'_>,
    repo: &ExportRepo,
) -> Result<u64, StoreError> {
    let rowid: Option<i64> = transaction
        .query_row(
            "SELECT id FROM repos WHERE forge = ?1 AND owner = ?2 AND name = ?3",
            params![repo.yagna.forge, repo.yagna.owner, repo.name],
            |row| row.get(0),
        )
        .optional()?;
    let Some(rowid) = rowid else {
        return Ok(0);
    };
    let mut added = 0u64;
    for tag in &repo.yagna.tags {
        transaction.execute(
            "INSERT OR IGNORE INTO tags (name, kind) VALUES (?1, ?2)",
            params![tag.name, tag.kind],
        )?;
        let tag_id: i64 = transaction.query_row(
            "SELECT id FROM tags WHERE name = ?1",
            params![tag.name],
            |row| row.get(0),
        )?;
        let attached = transaction.execute(
            "INSERT OR IGNORE INTO repo_tags (repo_rowid, tag_id) VALUES (?1, ?2)",
            params![rowid, tag_id],
        )?;
        added += attached as u64;
    }
    Ok(added)
}

/// The exported row back into the snapshot the store persists — the
/// shape the import writes. `None` when the row names no forge this
/// build knows.
fn snapshot_of(repo: &ExportRepo) -> Option<RepoSnapshot> {
    let forge = match repo.yagna.forge.as_str() {
        "github" => Forge::GitHub,
        _ => return None,
    };
    Some(RepoSnapshot {
        forge,
        repo_id: repo.id,
        owner: repo.yagna.owner.clone(),
        name: repo.name.clone(),
        description: repo.description.clone(),
        stars: repo.stargazers_count,
        languages: repo.yagna.languages.clone(),
        license: repo.yagna.license.clone(),
        topics: repo.topics.clone(),
        default_branch: repo.yagna.default_branch.clone(),
        created_at: repo.created_at,
        last_commit_at: repo.pushed_at,
        starred_at: repo.starred_at,
        commit_count: repo.yagna.commit_count,
        latest_release: repo.yagna.latest_release.clone(),
        is_fork: repo.fork,
        is_archived: repo.archived,
    })
}

/// The export's row mapping: one `EXPORT_SELECT` row into the export
/// shape.
fn map_export_repo(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportRepo> {
    let forge_text: String = row.get(1)?;
    let owner: String = row.get(2)?;
    let name: String = row.get(3)?;
    let topics_json: Option<String> = row.get(7)?;
    let languages_json: Option<String> = row.get(12)?;
    let license_name: Option<String> = row.get(13)?;
    let license_spdx: Option<String> = row.get(14)?;
    let latest_release_tag: Option<String> = row.get(16)?;
    let latest_release_at: Option<String> = row.get(17)?;
    let first_commit_date: Option<String> = row.get(20)?;
    let first_commit_source: Option<String> = row.get(21)?;
    let health_json: Option<String> = row.get(22)?;
    let missing_at: Option<String> = row.get(23)?;
    let tags_json: Option<String> = row.get(24)?;
    Ok(ExportRepo {
        id: row.get::<_, i64>(0)?.max(0) as u64,
        name: name.clone(),
        full_name: format!("{owner}/{name}"),
        html_url: html_url(&forge_text, &owner, &name),
        description: row.get(4)?,
        created_at: parse_time(row.get(8)?)?,
        pushed_at: parse_time(row.get(9)?)?,
        stargazers_count: row.get::<_, i64>(5)?.max(0) as u64,
        language: row.get(6)?,
        topics: parse_strings(topics_json),
        starred_at: match row.get::<_, Option<String>>(10)? {
            Some(text) => Some(parse_time(text)?),
            None => None,
        },
        fork: row.get::<_, i64>(18)? != 0,
        archived: row.get::<_, i64>(19)? != 0,
        yagna: ExportExtension {
            forge: forge_text,
            owner,
            default_branch: row.get(15)?,
            languages: parse_languages(languages_json),
            license: license_name.map(|name| LicenseInfo {
                name,
                spdx_id: license_spdx,
            }),
            commit_count: row.get::<_, i64>(11)?.max(0) as u64,
            latest_release: match latest_release_tag {
                Some(tag) => Some(ReleaseInfo {
                    tag,
                    published_at: match latest_release_at {
                        Some(text) => Some(parse_time(text)?),
                        None => None,
                    },
                }),
                None => None,
            },
            first_commit_at: match first_commit_date {
                Some(text) => Some(parse_time(text)?),
                None => None,
            },
            first_commit_source,
            missing: missing_at.is_some(),
            missing_at,
            tags: parse_tags(tags_json),
            health: match health_json {
                Some(text) => Some(parse_health(&text)?),
                None => None,
            },
        },
    })
}

/// The repository's page on its service.
fn html_url(forge: &str, owner: &str, name: &str) -> String {
    match forge {
        "github" => format!("https://github.com/{owner}/{name}"),
        other => format!("https://{other}/{owner}/{name}"),
    }
}

/// A stored timestamp back into an instant.
fn parse_time(text: String) -> rusqlite::Result<DateTime<Utc>> {
    let parsed = catalog::parse_stamp(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(parsed)
}

/// A stored JSON list of strings.
fn parse_strings(text: Option<String>) -> Vec<String> {
    text.and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

/// The stored language breakdown.
fn parse_languages(text: Option<String>) -> Vec<LanguageStat> {
    text.and_then(|text| serde_json::from_str::<Vec<LanguageStat>>(&text).ok())
        .unwrap_or_default()
}

/// The stored health breakdown.
fn parse_health(text: &str) -> rusqlite::Result<HealthReport> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

/// The folded tag array: `[{name, kind}]`, or nothing.
fn parse_tags(text: Option<String>) -> Vec<ExportTag> {
    let Some(text) = text else {
        return Vec::new();
    };
    let parsed: Vec<ExportTag> = serde_json::from_str(&text).unwrap_or_default();
    parsed
        .into_iter()
        .filter(|tag| !tag.name.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scratch, sample_snapshot};

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    /// A catalog with one repository, one tag and one category.
    fn seeded(label: &str) -> (Scratch, Store) {
        let (scratch, mut store) = store(label);
        store
            .upsert_snapshots(
                &[sample_snapshot("someone", "something")],
                date(2026, 10, 20),
            )
            .expect("seed");
        let rowid: i64 = store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get(0))
            .expect("rowid");
        store
            .assign_tag(rowid as u64, "reading list", None)
            .expect("tag");
        store
            .assign_tag(rowid as u64, "shelf", Some(crate::TagKind::Category))
            .expect("category");
        (scratch, store)
    }

    /// The export's id is the service's repository id — the catalog's
    /// own rowid is the library's bookkeeping and must never travel.
    #[test]
    fn the_export_carries_the_services_repository_id() {
        let (_scratch, mut store) = store("export-id");
        let mut snapshot = sample_snapshot("someone", "something");
        snapshot.repo_id = 987_654_321;
        store
            .upsert_snapshots(&[snapshot], date(2026, 10, 20))
            .expect("seed");
        let rowid: i64 = store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get(0))
            .expect("rowid");
        assert_ne!(rowid, 987_654_321, "the rowid is the catalog's own");
        let repos = store.export_repos().expect("export");
        assert_eq!(
            repos[0].id, 987_654_321,
            "the export carries the service's id"
        );
    }

    /// The export carries the catalog: the identity, the snapshot, the
    /// tags with their kinds, and the stored health.
    #[test]
    fn the_export_carries_the_catalog() {
        let (_scratch, store) = seeded("export-read");
        let repos = store.export_repos().expect("export");
        assert_eq!(repos.len(), 1);
        let repo = &repos[0];
        assert_eq!(repo.full_name, "someone/something");
        assert_eq!(repo.html_url, "https://github.com/someone/something");
        assert_eq!(repo.yagna.forge, "github");
        assert_eq!(repo.yagna.owner, "someone");
        let mut tags: Vec<(String, String)> = repo
            .yagna
            .tags
            .iter()
            .map(|tag| (tag.name.clone(), tag.kind.clone()))
            .collect();
        tags.sort();
        assert_eq!(
            tags,
            [
                ("reading list".to_string(), "tag".to_string()),
                ("shelf".to_string(), "category".to_string()),
            ]
        );
        assert!(repo.yagna.health.is_some(), "the health travels");
        assert!(repo.yagna.health.expect("health").total > 0);
    }

    /// The roundtrip: export, wipe, import, export again — the two
    /// exports are the same, and the import is idempotent.
    #[test]
    fn export_wipe_import_restores_the_catalog() {
        let (scratch, store) = seeded("export-roundtrip");
        let before = store.export_repos().expect("export");
        drop(store);

        // The wipe: a fresh database, the export's file the only copy.
        let fresh = Store::open(&scratch.path()).expect("fresh");
        fresh
            .conn
            .execute_batch("DELETE FROM repo_tags; DELETE FROM tags; DELETE FROM repos;")
            .expect("wipe");
        let mut fresh = fresh;
        let report = fresh
            .import_repos(&before, date(2026, 10, 30))
            .expect("import");
        assert_eq!(report.inserted, 1, "the row was new");
        assert_eq!(report.tagged, 2, "both tags rode along");

        let after = fresh.export_repos().expect("export");
        assert_eq!(after, before, "the catalog came back as it was");

        // The second import changes nothing.
        let again = fresh
            .import_repos(&before, date(2026, 10, 31))
            .expect("import again");
        assert_eq!(again.inserted, 0);
        assert_eq!(again.updated, 0);
        assert_eq!(again.unchanged, 1, "the row was already right");
        assert_eq!(again.tagged, 0, "the tags were already attached");
        let twice = fresh.export_repos().expect("export");
        assert_eq!(twice, before, "the second import changed nothing");
    }

    /// An import merges: the tags the user already had are kept, and
    /// the incoming ones are added beside them.
    #[test]
    fn an_import_merges_the_tags_without_clobbering() {
        let (_scratch, mut store) = seeded("export-merge");
        let mut exported = store.export_repos().expect("export");
        exported[0].yagna.tags.push(ExportTag {
            name: "new shelf".to_string(),
            kind: "tag".to_string(),
        });
        let report = store
            .import_repos(&exported, date(2026, 10, 30))
            .expect("import");
        assert_eq!(report.updated, 0, "nothing about the row changed");
        assert_eq!(report.tagged, 1, "only the new tag was attached");
        let rowid: i64 = store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get(0))
            .expect("rowid");
        let names: Vec<String> = store
            .conn
            .prepare(
                "SELECT t.name FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
                  WHERE rt.repo_rowid = ?1 ORDER BY t.name",
            )
            .and_then(|mut statement| {
                let rows = statement.query_map(params![rowid], |row| row.get(0))?;
                let mut names = Vec::new();
                for row in rows {
                    names.push(row?);
                }
                Ok(names)
            })
            .expect("tags");
        assert_eq!(names, ["new shelf", "reading list", "shelf"]);
    }

    /// The SQL format: a consistent copy of the database, readable on
    /// its own.
    #[test]
    fn the_backup_is_a_readable_copy() {
        let (scratch, store) = seeded("export-backup");
        let copy = scratch.path().with_extension("copy.db");
        store.backup_to(&copy).expect("backup");
        let copied = Store::open(&copy).expect("open the copy");
        let repos = copied.export_repos().expect("export");
        assert_eq!(repos.len(), 1, "the copy holds the catalog");
        assert_eq!(repos[0].full_name, "someone/something");
    }

    /// A row naming a forge this build does not know is skipped, not
    /// guessed at — and the skip is counted, not silent.
    #[test]
    fn an_unknown_forge_is_skipped_and_counted() {
        let (_scratch, mut store) = seeded("export-unknown-forge");
        let mut exported = store.export_repos().expect("export");
        exported[0].yagna.forge = "gitlab".to_string();
        let report = store
            .import_repos(&exported, date(2026, 10, 30))
            .expect("import");
        assert_eq!(report.inserted, 0);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.skipped, 1, "the skip is visible in the report");
        assert_eq!(store.export_repos().expect("export").len(), 1);
    }

    /// A foreign file without the first-commit fact still imports: the
    /// column is NOT NULL, and the honest default is written.
    #[test]
    fn an_import_without_the_first_commit_fact_uses_the_default() {
        let (_scratch, mut store) = store("export-no-first-commit");
        let mut exported = store.export_repos().expect("export");
        exported.clear();
        exported.push(ExportRepo {
            id: 5,
            name: "thing".to_string(),
            full_name: "someone/thing".to_string(),
            html_url: "https://github.com/someone/thing".to_string(),
            description: None,
            created_at: date(2020, 1, 1),
            pushed_at: date(2026, 1, 1),
            stargazers_count: 3,
            language: None,
            topics: Vec::new(),
            starred_at: None,
            fork: false,
            archived: false,
            yagna: ExportExtension {
                forge: "github".to_string(),
                owner: "someone".to_string(),
                ..ExportExtension::default()
            },
        });
        let report = store
            .import_repos(&exported, date(2026, 10, 30))
            .expect("import");
        assert_eq!(report.inserted, 1);
        let source: String = store
            .conn
            .query_row("SELECT first_commit_source FROM repos", [], |row| {
                row.get(0)
            })
            .expect("the row wrote its default");
        assert_eq!(source, "repo-created");
    }

    /// A missing repository's history survives imports: the carried
    /// "gone since" stamp is written once and never replaced by the
    /// import's own clock, so re-imports stay idempotent.
    #[test]
    fn a_missing_repository_keeps_its_gone_since_stamp() {
        let (scratch, store) = seeded("export-missing-at");
        // The repository goes missing, with its stamp.
        store
            .conn
            .execute("UPDATE repos SET missing_at = '2026-09-01T00:00:00Z'", [])
            .expect("missing");
        let exported = store.export_repos().expect("export");
        assert_eq!(
            exported[0].yagna.missing_at.as_deref(),
            Some("2026-09-01T00:00:00Z"),
            "the export carries the stamp"
        );
        drop(store);

        let mut again = Store::open(&scratch.path()).expect("reopen");
        let first = again
            .import_repos(&exported, date(2026, 10, 30))
            .expect("import");
        assert_eq!(first.updated, 0, "the stamp did not move under the import");
        let stamp: Option<String> = again
            .conn
            .query_row("SELECT missing_at FROM repos", [], |row| row.get(0))
            .expect("the stamp");
        assert_eq!(stamp.as_deref(), Some("2026-09-01T00:00:00Z"));
        let second = again
            .import_repos(&exported, date(2026, 10, 31))
            .expect("import again");
        assert_eq!(second.updated, 0, "and the second import is a no-op too");
    }
}
