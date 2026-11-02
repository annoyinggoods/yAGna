//! The repos table: upserts from the sweep, missing-marking, the counts
//! the CLI reports, and the read side the list UI renders through.
//!
//! The store computes nothing of its own — it persists the snapshot and
//! the health score the core's pure `health` function assigns it, and it
//! writes the first-commit policy's cheap default (`created_at` as the
//! first-commit date, source `repo-created`) on insert. One
//! `upsert_snapshots` call is one transaction: a killed run loses at
//! most one page, and upserts are idempotent against the
//! `(forge, owner, name)` key, so overlap between sweeps is a non-event.
//! A repository absent from a sweep keeps its row with `missing_at` set
//! — the catalog is append-mostly history. The read side (`page`,
//! `total_count`) projects the same table into `RepoRow`s in one SELECT:
//! the README excerpt and the user's tags arrive via joins, never per-row
//! follow-up queries. The filter's halves are the selected tags and the
//! one GitHub topic the library is narrowed to; `topic_counts`
//! aggregates the catalog's own topics for the sidebar's drawer.
//!
//! Items: `CatalogOverview` — the counts the stats line prints;
//! `RescoreReport` — what one fit re-score did; `stamp`, `parse_stamp`
//! — the one timestamp format every column uses.
//! Depends on: yagna_core (the records persisted, the health math),
//! chrono (timestamps).
//! Called by: crate::lib (the Store facade) and the sync verb.

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use yagna_core::health::{HealthReport, health};
use yagna_core::model::{Forge, RepoRef, RepoRow, RepoSnapshot, SortKey};

use crate::userstats::NameCount;
use crate::{Store, StoreError};

/// How a tag filter combines the selected tags: every one of them
/// (the default) or any one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TagMatch {
    /// The repository must carry every selected tag.
    #[default]
    All,
    /// The repository must carry at least one of the selected tags.
    Any,
}

/// The filter the page query honors: the tags the user selected, how
/// they combine, and the GitHub topic the library is narrowed to. An
/// empty selection and no topic filter nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogFilter {
    pub tag_ids: Vec<i64>,
    pub tag_match: TagMatch,
    /// The one GitHub topic the library is narrowed to — a fact about
    /// the stars, read from the repos the sweep carried.
    pub topic: Option<String>,
}

impl CatalogFilter {
    /// A filter that lets everything through.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether the filter narrows anything at all.
    pub fn is_empty(&self) -> bool {
        self.tag_ids.is_empty() && self.topic.is_none()
    }
}

/// One repository's search-index row: the fields the fuzzy haystack is
/// built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    pub repo_rowid: i64,
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub topics: Vec<String>,
    pub tags: Vec<String>,
}

/// What one fit re-score did: how many rows were re-scored, and how
/// many of them changed score (a first score counts as a move).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RescoreReport {
    pub scored: u64,
    pub moved: u64,
}

/// Whether a re-score moved the row: no previous score is a move, and
/// a changed score is a move. Unchanged is not.
fn score_moved(previous: Option<f64>, total: f64) -> bool {
    match previous {
        None => true,
        Some(previous) => (previous - total).abs() > 1e-9,
    }
}

/// The upsert: insert, or refresh every sweep-carried field when the
/// identity already exists. `first_seen_at` is written on insert only —
/// the first time this repository was ever seen survives updates — the
/// `first_commit_*` pair is likewise insert-only (the enriched value a
/// later part writes must survive a re-upsert), and `missing_at` is
/// cleared: a repository the sweep saw is back.
const UPSERT: &str = "
INSERT INTO repos (
  forge, repo_id, owner, name, description, stars,
  primary_language, languages_json,
  license_name, license_spdx,
  topics_json, default_branch,
  created_at, starred_at, last_commit_at,
  first_commit_date, first_commit_source, commit_count,
  latest_release_tag, latest_release_at,
  is_fork, is_archived, health_score, health_json,
  first_seen_at, last_synced_at
) VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
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
  commit_count = excluded.commit_count,
  latest_release_tag = excluded.latest_release_tag,
  latest_release_at = excluded.latest_release_at,
  is_fork = excluded.is_fork,
  is_archived = excluded.is_archived,
  health_score = excluded.health_score,
  health_json = excluded.health_json,
  last_synced_at = excluded.last_synced_at,
  missing_at = NULL
";

impl Store {
    /// Upserts one sweep page — every snapshot in one transaction — and
    /// returns the number of rows written. `now` stamps `last_synced_at`
    /// (and `first_seen_at` on first sight); it is the sweep's start
    /// instant, the same for every page of one run. Health is the core's
    /// pure score of each snapshot against that same instant.
    pub fn upsert_snapshots(
        &mut self,
        page: &[RepoSnapshot],
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        // Immediate: the sweep reads before it writes, and a deferred
        // read snapshot upgrades into BUSY_SNAPSHOT under a second
        // process — the write lock is taken up front instead.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut written = 0u64;
        {
            let mut statement = transaction.prepare(UPSERT)?;
            for snapshot in page {
                let score = health(snapshot, now);
                // The digest's staleness is decided before the row moves:
                // after the upsert the stored stamp is already the new one.
                let incoming_commit = stamp(snapshot.last_commit_at);
                let previous = crate::digests::stored_last_commit(
                    &transaction,
                    snapshot.forge.as_str(),
                    &snapshot.owner,
                    &snapshot.name,
                );
                written += statement.execute(params![
                    snapshot.forge.as_str(),
                    saturating_i64(snapshot.repo_id),
                    snapshot.owner,
                    snapshot.name,
                    snapshot.description,
                    saturating_i64(snapshot.stars),
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
                    stamp(snapshot.created_at),
                    snapshot.starred_at.map(stamp),
                    stamp(snapshot.last_commit_at),
                    stamp(snapshot.created_at),
                    FIRST_COMMIT_SOURCE_DEFAULT,
                    saturating_i64(snapshot.commit_count),
                    snapshot
                        .latest_release
                        .as_ref()
                        .map(|release| release.tag.as_str()),
                    snapshot
                        .latest_release
                        .as_ref()
                        .and_then(|release| release.published_at.map(stamp)),
                    i64::from(snapshot.is_fork),
                    i64::from(snapshot.is_archived),
                    i64::from(score.total),
                    serde_json::to_string(&score)?,
                    stamp(now),
                    stamp(now),
                ])? as u64;
                if let Some((rowid, stored_commit)) = previous
                    && stored_commit != incoming_commit
                {
                    crate::digests::stamp_stale(&transaction, rowid, &stamp(now));
                }
            }
        }
        transaction.commit()?;
        Ok(written)
    }

    /// Marks every repository the sweep did not see as missing:
    /// `missing_at = now` on rows never marked before whose identity is
    /// not in `seen` (the `(owner, name)` pairs the sweep produced).
    /// Returns how many rows went missing.
    pub fn mark_missing(
        &mut self,
        seen: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let transaction = self.conn.transaction()?;
        transaction.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS seen_identities (owner TEXT NOT NULL, name TEXT NOT NULL);
             DELETE FROM seen_identities",
        )?;
        {
            let mut statement =
                transaction.prepare("INSERT INTO seen_identities (owner, name) VALUES (?1, ?2)")?;
            for (owner, name) in seen {
                statement.execute(params![owner, name])?;
            }
        }
        let marked = transaction.execute(
            "UPDATE repos SET missing_at = ?1
             WHERE forge = 'github'
               AND missing_at IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM seen_identities
                 WHERE seen_identities.owner = repos.owner
                   AND seen_identities.name = repos.name
               )",
            params![stamp(now)],
        )?;
        transaction.commit()?;
        Ok(marked as u64)
    }

    /// How many rows carry `first_seen_at` at exactly `instant` — the
    /// repositories one sweep (whose start was that instant) added.
    pub fn count_first_seen_at(&self, instant: DateTime<Utc>) -> Result<u64, StoreError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM repos WHERE first_seen_at = ?1",
            params![stamp(instant)],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as u64)
    }

    /// The counts behind the stats line: everything the catalog says
    /// about itself in one query.
    pub fn overview(&self) -> Result<CatalogOverview, StoreError> {
        self.conn
            .query_row(OVERVIEW, [], |row| {
                let total: i64 = row.get(0)?;
                let present: i64 = row.get(1)?;
                let missing: i64 = row.get(2)?;
                let forks: i64 = row.get(3)?;
                let archived: i64 = row.get(4)?;
                let health_min: Option<i64> = row.get(5)?;
                let health_average: Option<f64> = row.get(6)?;
                let health_max: Option<i64> = row.get(7)?;
                let health_unscored: i64 = row.get(8)?;
                let stars_total: i64 = row.get(9)?;
                let tagged_repos: i64 = row.get(10)?;
                Ok(CatalogOverview {
                    repos_total: total.max(0) as u64,
                    repos_present: present.max(0) as u64,
                    repos_missing: missing.max(0) as u64,
                    forks: forks.max(0) as u64,
                    archived: archived.max(0) as u64,
                    health_min: health_min.map(|value| value.max(0) as u64),
                    health_average,
                    health_max: health_max.map(|value| value.max(0) as u64),
                    health_unscored: health_unscored.max(0) as u64,
                    stars_total: stars_total.max(0) as u64,
                    tagged_repos: tagged_repos.max(0) as u64,
                })
            })
            .map_err(StoreError::from)
    }

    /// One page of the catalog as `RepoRow`s: `limit` rows starting at
    /// `offset`, ordered by `sort` (each key's `order_by` carries the
    /// stable tiebreak, so equal keys never reshuffle between pages).
    /// Missing repositories are excluded; the README excerpt and the
    /// user's tags arrive in the same SELECT. `filter` narrows the page
    /// to the repositories carrying the selected tags.
    pub fn page(
        &self,
        sort: SortKey,
        offset: u32,
        limit: u32,
        filter: &CatalogFilter,
    ) -> Result<Vec<RepoRow>, StoreError> {
        let (clause, mut values) = filter_clause(filter);
        let limit_index = values.len() + 1;
        let offset_index = values.len() + 2;
        values.push(Value::Integer(i64::from(limit)));
        values.push(Value::Integer(i64::from(offset)));
        let sql = format!(
            "{PAGE_SELECT} WHERE r.missing_at IS NULL {clause} ORDER BY {} LIMIT ?{limit_index} OFFSET ?{offset_index}",
            sort.order_by()
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), map_page_row)?;
        let mut page = Vec::new();
        for row in rows {
            page.push(row?);
        }
        Ok(page)
    }

    /// How many repositories the catalog currently shows under a filter:
    /// every row that is not missing and that carries the selected tags
    /// — the same population `page` draws from.
    pub fn total_count(&self, filter: &CatalogFilter) -> Result<u64, StoreError> {
        let (clause, values) = filter_clause(filter);
        let sql = format!("SELECT COUNT(*) FROM repos r WHERE r.missing_at IS NULL {clause}");
        let count = self
            .conn
            .query_row(&sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count.max(0) as u64)
    }

    /// Every repository id under a filter, in the sort order — the
    /// candidate set a fuzzy search ranks. The ids are the catalog's own
    /// row ids, so the ranked set can be sliced and fetched back.
    pub fn candidate_ids(
        &self,
        sort: SortKey,
        filter: &CatalogFilter,
    ) -> Result<Vec<i64>, StoreError> {
        let (clause, values) = filter_clause(filter);
        let sql = format!(
            "SELECT r.id FROM repos r WHERE r.missing_at IS NULL {clause} ORDER BY {}",
            sort.order_by()
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows =
            statement.query_map(params_from_iter(values.iter()), |row| row.get::<_, i64>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// The full rows for a set of row ids, in the order the ids were
    /// given. Ids that name no present repository are skipped.
    pub fn rows_by_ids(&self, ids: &[i64]) -> Result<Vec<RepoRow>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = (1..=ids.len()).map(|index| format!("?{index}")).collect();
        let sql = format!(
            "{PAGE_SELECT} WHERE r.missing_at IS NULL AND r.id IN ({})",
            placeholders.join(", ")
        );
        let values: Vec<Value> = ids.iter().map(|id| Value::Integer(*id)).collect();
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), map_page_row)?;
        let mut found = Vec::new();
        for row in rows {
            found.push(row?);
        }
        let mut ordered = Vec::with_capacity(found.len());
        for id in ids {
            if let Some(row) = found.iter().find(|row| row.repo_rowid == *id) {
                ordered.push(row.clone());
            }
        }
        Ok(ordered)
    }

    /// Every repository's search-index row: the fields the fuzzy
    /// haystack is built from, in one read. Missing repositories are
    /// excluded — the index covers what the list can show.
    pub fn search_index_rows(&self) -> Result<Vec<SearchRow>, StoreError> {
        let sql = format!("{SEARCH_SELECT} WHERE r.missing_at IS NULL");
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([], map_search_row)?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(row?);
        }
        Ok(rows_out)
    }

    /// One repository's search-index row — the tag-edit path, which
    /// patches that row's haystack instead of rebuilding the index.
    pub fn search_index_row(&self, repo_rowid: i64) -> Result<Option<SearchRow>, StoreError> {
        let sql = format!("{SEARCH_SELECT} WHERE r.missing_at IS NULL AND r.id = ?1");
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query(params![repo_rowid])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(map_search_row(row)?)),
        }
    }

    /// The user's tag names for one repository row, sorted
    /// case-insensitively — the single-repo form of what `page` folds
    /// into every row.
    pub fn tag_names(&self, repo_rowid: i64) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.name
               FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
              WHERE rt.repo_rowid = ?1
              ORDER BY t.name COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map(params![repo_rowid], |row| row.get::<_, String>(0))?;
        let mut names = Vec::new();
        for name in rows {
            names.push(name?);
        }
        Ok(names)
    }

    /// Finds a repository's row id by its identity.
    pub fn repo_rowid(
        &self,
        forge: &str,
        owner: &str,
        name: &str,
    ) -> Result<Option<i64>, StoreError> {
        self.conn
            .query_row(
                "SELECT id FROM repos WHERE forge = ?1 AND owner = ?2 AND name = ?3",
                params![forge, owner, name],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Assembles one repository's fit-score inputs from the store's
    /// columns and joins. `now` anchors every age calculation.
    pub fn fit_facts_for_repo(
        &self,
        repo_rowid: i64,
        now: DateTime<Utc>,
    ) -> Result<yagna_core::fit::FitFacts, StoreError> {
        fit_facts_from_conn(&self.conn, repo_rowid, now)
    }

    /// Recomputes the fit score for every present repository in one
    /// transaction: how many rows were re-scored, and how many of them
    /// moved (a first score counts as a move).
    pub fn rescore_fit(
        &mut self,
        spec: &yagna_core::fit::FitSpec,
        now: DateTime<Utc>,
    ) -> Result<RescoreReport, StoreError> {
        let rows = self.rows_to_score()?;
        let transaction = self.conn.transaction()?;
        let mut report = RescoreReport::default();
        {
            let mut update = transaction
                .prepare("UPDATE repos SET fit_score = ?1, fit_json = ?2 WHERE id = ?3")?;
            for (id, previous) in rows {
                let facts = fit_facts_from_conn(&transaction, id, now)?;
                let scored = yagna_core::fit::score(spec, &facts);
                let json = serde_json::to_string(&scored)?;
                update.execute(params![scored.total, json, id])?;
                report.scored += 1;
                if score_moved(previous, scored.total) {
                    report.moved += 1;
                }
            }
        }
        transaction.commit()?;
        Ok(report)
    }

    /// Recomputes the fit score of one repository alone — the single
    /// row's re-score after a live refresh. Returns whether the row
    /// moved.
    pub fn rescore_fit_row(
        &mut self,
        repo_rowid: i64,
        spec: &yagna_core::fit::FitSpec,
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let previous = self
            .conn
            .query_row(
                "SELECT fit_score FROM repos WHERE id = ?1",
                params![repo_rowid],
                |row| row.get::<_, Option<f64>>(0),
            )
            .optional()?
            .flatten();
        let facts = fit_facts_from_conn(&self.conn, repo_rowid, now)?;
        let scored = yagna_core::fit::score(spec, &facts);
        let json = serde_json::to_string(&scored)?;
        self.conn.execute(
            "UPDATE repos SET fit_score = ?1, fit_json = ?2 WHERE id = ?3",
            params![scored.total, json, repo_rowid],
        )?;
        Ok(u64::from(score_moved(previous, scored.total)))
    }

    /// Reads one repository's stored fit score and breakdown.
    pub fn fit_score_of(&self, repo_rowid: i64) -> Result<Option<(f64, String)>, StoreError> {
        self.conn
            .query_row(
                "SELECT fit_score, fit_json FROM repos WHERE id = ?1 AND fit_score IS NOT NULL",
                params![repo_rowid],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// The reader's star time for one repository — the fact a live
    /// refresh must never overwrite.
    pub fn starred_at_of(&self, repo_rowid: i64) -> Result<Option<DateTime<Utc>>, StoreError> {
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT starred_at FROM repos WHERE id = ?1",
                params![repo_rowid],
                |row| row.get(0),
            )
            .optional()?;
        stored.flatten().map(|text| parse_stamp(&text)).transpose()
    }

    /// Every present repository's row id with its stored fit score (if
    /// any), in a stable order — the re-score's work list.
    fn rows_to_score(&self) -> Result<Vec<(i64, Option<f64>)>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, fit_score FROM repos WHERE missing_at IS NULL ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Reads one health JSON column back — the tests' window into what
    /// an upsert persisted.
    #[cfg(test)]
    pub(crate) fn health_json_of(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<HealthReport, StoreError> {
        let text: String = self.conn.query_row(
            "SELECT health_json FROM repos WHERE owner = ?1 AND name = ?2",
            params![owner, name],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    }
}
/// The first-commit policy's cheap default, written on insert: the
/// repository's creation as its first-commit date.
pub const FIRST_COMMIT_SOURCE_DEFAULT: &str = "repo-created";

/// The read side's SELECT: every column a `RepoRow` needs, the README
/// excerpt joined from `readmes`, and the user's tags folded in as one
/// JSON array subquery — one SELECT serves the whole page, never N+1.
/// The `WHERE` clause is appended by the caller (the missing-rows
/// exclusion and the tag filter); the ORDER BY is appended per key.
const PAGE_SELECT: &str = "
SELECT
  r.id, r.forge, r.owner, r.name, r.description, r.stars,
  r.primary_language, r.license_name, r.topics_json,
  r.created_at, r.starred_at, r.last_commit_at, r.first_commit_date,
  r.health_json,
  r.fit_score,
  COALESCE(m.excerpt, ''),
  (SELECT json_group_array(t.name)
     FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
    WHERE rt.repo_rowid = r.id) AS tags_json
FROM repos r
LEFT JOIN readmes m ON m.repo_rowid = r.id
";

/// The search index's SELECT: the row id and the fields the haystack
/// is built from, in one read.
const SEARCH_SELECT: &str = "
SELECT r.id, r.owner, r.name, r.description, r.topics_json,
       COALESCE((SELECT json_group_array(t.name)
                   FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
                  WHERE rt.repo_rowid = r.id), '[]') AS tags_json
FROM repos r
";

/// The column indexes of `PAGE_SELECT`, kept in one place so the mapper
/// and the query cannot drift apart.
mod page_col {
    pub(crate) const ROWID: usize = 0;
    pub(crate) const FORGE: usize = 1;
    pub(crate) const OWNER: usize = 2;
    pub(crate) const NAME: usize = 3;
    pub(crate) const DESCRIPTION: usize = 4;
    pub(crate) const STARS: usize = 5;
    pub(crate) const PRIMARY_LANGUAGE: usize = 6;
    pub(crate) const LICENSE_NAME: usize = 7;
    pub(crate) const TOPICS_JSON: usize = 8;
    pub(crate) const CREATED_AT: usize = 9;
    pub(crate) const STARRED_AT: usize = 10;
    pub(crate) const LAST_COMMIT_AT: usize = 11;
    pub(crate) const FIRST_COMMIT_DATE: usize = 12;
    pub(crate) const HEALTH_JSON: usize = 13;
    pub(crate) const FIT_SCORE: usize = 14;
    pub(crate) const EXCERPT: usize = 15;
    pub(crate) const TAGS_JSON: usize = 16;
}

/// The tag filter's SQL fragment and its parameters: the repositories
/// carrying every selected tag (the default) or any of them, and — when
/// a topic is named — the repositories whose own topics carry it. The
/// topic match is exact over the JSON array's elements, never a
/// substring of the column.
fn filter_clause(filter: &CatalogFilter) -> (String, Vec<Value>) {
    let mut clause = String::new();
    let mut values: Vec<Value> = Vec::new();
    if !filter.tag_ids.is_empty() {
        let placeholders: Vec<String> = (1..=filter.tag_ids.len())
            .map(|index| format!("?{index}"))
            .collect();
        values.extend(filter.tag_ids.iter().map(|id| Value::Integer(*id)));
        let list = placeholders.join(", ");
        clause.push_str(&match filter.tag_match {
            TagMatch::Any => {
                format!("AND r.id IN (SELECT repo_rowid FROM repo_tags WHERE tag_id IN ({list}))")
            }
            TagMatch::All => format!(
                "AND r.id IN (SELECT repo_rowid FROM repo_tags WHERE tag_id IN ({list}) \
                 GROUP BY repo_rowid HAVING COUNT(DISTINCT tag_id) = {})",
                filter.tag_ids.len()
            ),
        });
    }
    let Some(topic) = filter
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
    else {
        return (clause, values);
    };
    let topic_index = values.len() + 1;
    values.push(Value::Text(topic.to_string()));
    clause.push_str(&format!(
        "AND EXISTS (SELECT 1 FROM json_each(r.topics_json) je WHERE je.value = ?{topic_index})"
    ));
    (clause, values)
}

/// The topics of the catalog, aggregated with their counts — the
/// sidebar's topic drawer. The filter's tag half narrows the count (a
/// topic says how many of the *filtered* repositories carry it); the
/// filter's own topic is deliberately ignored here, so the selected
/// topic keeps its true count and the others show what the rest of the
/// filter leaves them.
pub fn topic_counts(store: &Store, filter: &CatalogFilter) -> Result<Vec<NameCount>, StoreError> {
    let tag_only = CatalogFilter {
        topic: None,
        ..filter.clone()
    };
    let (clause, values) = filter_clause(&tag_only);
    let sql = format!(
        "SELECT je.value AS topic, COUNT(*) AS carries
           FROM repos r, json_each(r.topics_json) je
          WHERE r.missing_at IS NULL AND r.topics_json IS NOT NULL {clause}
          GROUP BY topic
         ORDER BY carries DESC, topic ASC"
    );
    let mut statement = store.conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        let count: i64 = row.get(1)?;
        Ok(NameCount {
            name: row.get(0)?,
            count: count.max(0) as u64,
        })
    })?;
    let mut topics = Vec::new();
    for row in rows {
        topics.push(row?);
    }
    Ok(topics)
}

/// The catalog's self-description: repo counts by presence and shape,
/// the spread of the stored health scores, and the two numbers the USER
/// tab's summary opens with (the stars total, the tagged count).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogOverview {
    pub repos_total: u64,
    pub repos_present: u64,
    pub repos_missing: u64,
    pub forks: u64,
    pub archived: u64,
    pub health_min: Option<u64>,
    pub health_average: Option<f64>,
    pub health_max: Option<u64>,
    pub health_unscored: u64,
    pub stars_total: u64,
    pub tagged_repos: u64,
}

const OVERVIEW: &str = "
SELECT
  (SELECT COUNT(*) FROM repos),
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NULL),
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NOT NULL),
  (SELECT COUNT(*) FROM repos WHERE is_fork = 1),
  (SELECT COUNT(*) FROM repos WHERE is_archived = 1),
  (SELECT MIN(health_score) FROM repos WHERE health_score IS NOT NULL),
  (SELECT AVG(health_score) FROM repos WHERE health_score IS NOT NULL),
  (SELECT MAX(health_score) FROM repos WHERE health_score IS NOT NULL),
  (SELECT COUNT(*) FROM repos WHERE health_score IS NULL),
  (SELECT COALESCE(SUM(stars), 0) FROM repos WHERE missing_at IS NULL),
  (SELECT COUNT(DISTINCT rt.repo_rowid) FROM repo_tags rt
     JOIN repos r ON r.id = rt.repo_rowid
     WHERE r.missing_at IS NULL)
";

/// The one timestamp format every date column uses: UTC RFC 3339 at
/// second precision, so lexicographic order is chronological.
pub(crate) fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The inverse of `stamp`: a stored date column back into an instant.
pub(crate) fn parse_stamp(text: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| {
            StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            ))
        })
}

/// The counts fit SQLite integers by far; a value that would not is
/// clamped rather than wrapped.
pub(crate) fn saturating_i64(value: u64) -> i64 {
    crate::saturating_rowid(value)
}

/// Maps one `PAGE_SELECT` row into a `RepoRow`. Every parse failure is a
/// `StoreError::Parse`: the columns were written by this store, so a
/// value that does not read back means the stored data is not what the
/// schema promises.
fn map_page_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepoRow> {
    map_page_row_inner(row).map_err(store_error_to_rusqlite)
}

fn map_page_row_inner(row: &rusqlite::Row<'_>) -> Result<RepoRow, StoreError> {
    let forge_text: String = row.get(page_col::FORGE)?;
    let forge = match forge_text.as_str() {
        "github" => Forge::GitHub,
        other => return Err(unreadable(&format!("forge {other:?}"))),
    };
    let created_at: String = row.get(page_col::CREATED_AT)?;
    let starred_at: Option<String> = row.get(page_col::STARRED_AT)?;
    let last_commit_at: String = row.get(page_col::LAST_COMMIT_AT)?;
    let first_commit_date: Option<String> = row.get(page_col::FIRST_COMMIT_DATE)?;
    let health_json: Option<String> = row.get(page_col::HEALTH_JSON)?;
    let tags_json: Option<String> = row.get(page_col::TAGS_JSON)?;
    let topics_json: Option<String> = row.get(page_col::TOPICS_JSON)?;
    let topics = topics_json
        .map(|text| serde_json::from_str::<Vec<String>>(&text))
        .transpose()?
        .unwrap_or_default();
    Ok(RepoRow {
        repo_rowid: row.get(page_col::ROWID)?,
        repo_ref: RepoRef {
            forge,
            owner: row.get(page_col::OWNER)?,
            name: row.get(page_col::NAME)?,
        },
        description: row.get(page_col::DESCRIPTION)?,
        stars: row.get::<_, i64>(page_col::STARS)?.max(0) as u64,
        primary_language: row.get(page_col::PRIMARY_LANGUAGE)?,
        license_name: row.get(page_col::LICENSE_NAME)?,
        topics,
        created_at: parse_stamp(&created_at)?,
        starred_at: match starred_at {
            Some(text) => Some(parse_stamp(&text)?),
            None => None,
        },
        last_commit_at: parse_stamp(&last_commit_at)?,
        first_commit_date: match first_commit_date {
            Some(text) => Some(parse_stamp(&text)?),
            None => None,
        },
        health: parse_health(health_json)?,
        fit_score: row.get(page_col::FIT_SCORE)?,
        tags: parse_tags(tags_json)?,
        excerpt: row.get(page_col::EXCERPT)?,
    })
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

/// Parses a stored health JSON column. An absent column defaults to the
/// zero report — the schema allows the NULL — while a present-but-unreadable
/// one is a `StoreError::Parse`.
fn parse_health(text: Option<String>) -> Result<HealthReport, StoreError> {
    match text {
        None => Ok(HealthReport::default()),
        Some(text) => serde_json::from_str(&text)
            .map_err(|error| unreadable(&format!("health json: {error}"))),
    }
}

/// Parses the page query's folded tag array. An empty aggregation reads
/// back as JSON `[]` (or, on engines without that guarantee, NULL) —
/// both mean no tags; anything else unreadable is a `StoreError::Parse`.
fn parse_tags(text: Option<String>) -> Result<Vec<String>, StoreError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let mut tags = serde_json::from_str::<Vec<String>>(&text)
        .map_err(|error| unreadable(&format!("tags json: {error}")))?;
    tags.retain(|tag| !tag.trim().is_empty());
    tags.sort_by_key(|tag| tag.to_lowercase());
    Ok(tags)
}

/// A `StoreError::Parse` for a stored value that does not read back.
fn unreadable(what: &str) -> StoreError {
    StoreError::Parse(format!("cannot read {what} from the catalog"))
}

/// Maps one `SEARCH_SELECT` row into a search row: the identity, the
/// description, the topics and the user's tags.
fn map_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchRow> {
    let topics_json: Option<String> = row.get(4)?;
    let tags_json: Option<String> = row.get(5)?;
    Ok(SearchRow {
        repo_rowid: row.get(0)?,
        owner: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        topics: parse_string_list(topics_json).map_err(store_error_to_rusqlite)?,
        tags: parse_string_list(tags_json).map_err(store_error_to_rusqlite)?,
    })
}

/// Parses a stored JSON array of strings; an absent column is an empty
/// list, an unreadable one a store error.
fn parse_string_list(text: Option<String>) -> Result<Vec<String>, StoreError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let mut values: Vec<String> = serde_json::from_str(&text)
        .map_err(|error| unreadable(&format!("a json list: {error}")))?;
    values.retain(|value| !value.trim().is_empty());
    Ok(values)
}

/// One repository's raw fit-fact columns before date arithmetic.
struct FitFactsRow {
    stars: i64,
    commit_count: i64,
    created_at: String,
    last_commit_at: String,
    starred_at: Option<String>,
    health_score: i64,
    license_spdx: Option<String>,
    primary_language: Option<String>,
    topics_json: Option<String>,
    is_fork: i64,
    is_archived: i64,
    latest_release_tag: Option<String>,
    latest_release_at: Option<String>,
    readme_present: i64,
    digest_present: i64,
    local: i64,
    tagged: i64,
}

/// The one SELECT that gathers every fit-fact column and join flag.
const FIT_FACTS_SELECT: &str = "
SELECT r.stars, r.commit_count, r.created_at, r.last_commit_at, r.starred_at,
       r.health_score, r.license_spdx, r.primary_language, r.topics_json,
       r.is_fork, r.is_archived, r.latest_release_tag, r.latest_release_at,
       EXISTS(SELECT 1 FROM readmes m WHERE m.repo_rowid = r.id),
       EXISTS(SELECT 1 FROM digests d WHERE d.repo_rowid = r.id),
       EXISTS(SELECT 1 FROM local_repos lr
               WHERE lr.owner = r.owner AND lr.repo_name = r.name),
       EXISTS(SELECT 1 FROM repo_tags rt WHERE rt.repo_rowid = r.id)
  FROM repos r WHERE r.id = ?1";

/// Maps one fit-facts row from SQLite.
fn map_fit_facts_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FitFactsRow> {
    Ok(FitFactsRow {
        stars: row.get(0)?,
        commit_count: row.get(1)?,
        created_at: row.get(2)?,
        last_commit_at: row.get(3)?,
        starred_at: row.get(4)?,
        health_score: row.get(5)?,
        license_spdx: row.get(6)?,
        primary_language: row.get(7)?,
        topics_json: row.get(8)?,
        is_fork: row.get(9)?,
        is_archived: row.get(10)?,
        latest_release_tag: row.get(11)?,
        latest_release_at: row.get(12)?,
        readme_present: row.get(13)?,
        digest_present: row.get(14)?,
        local: row.get(15)?,
        tagged: row.get(16)?,
    })
}

/// Turns one raw row into the core's `FitFacts`, computing every age
/// against `now`.
fn build_fit_facts(row: FitFactsRow, now: DateTime<Utc>) -> yagna_core::fit::FitFacts {
    let created = parse_stamp(&row.created_at).ok();
    let last_commit = parse_stamp(&row.last_commit_at).ok();
    let starred = row.starred_at.as_deref().and_then(|s| parse_stamp(s).ok());
    let release_at = row
        .latest_release_at
        .as_deref()
        .and_then(|s| parse_stamp(s).ok());
    yagna_core::fit::FitFacts {
        recency_days: last_commit.map(|at| (now - at).num_days() as f64),
        created_days: created.map(|at| (now - at).num_days() as f64),
        stars: row.stars.max(0) as u64,
        commit_count: row.commit_count.max(0) as u64,
        commits_per_year: commits_per_year(row.commit_count, created, now),
        release_age_days: release_at.map(|at| (now - at).num_days() as f64),
        release_major: row
            .latest_release_tag
            .as_deref()
            .and_then(yagna_core::health::parse_semver)
            .map(|(major, _, _)| major),
        license_class: Some(license_class(row.license_spdx.as_deref()).to_string()),
        language: row.primary_language,
        topics: parse_string_list(row.topics_json).unwrap_or_default(),
        health: row.health_score.clamp(0, 12) as u8,
        readme_present: row.readme_present != 0,
        digest_present: row.digest_present != 0,
        is_fork: row.is_fork != 0,
        is_archived: row.is_archived != 0,
        local: row.local != 0,
        tagged: row.tagged != 0,
        starred_days: starred.map(|at| (now - at).num_days() as f64),
    }
}

/// Commits per year of age: the cadence the health index already
/// divides. A repo with no commits or no representable age scores zero.
fn commits_per_year(
    commits: i64,
    created: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<f64> {
    let commits = commits.max(0) as f64;
    if commits == 0.0 {
        return Some(0.0);
    }
    let created = created?;
    let days = (now - created).num_days().max(1) as f64;
    Some(commits / (days / 365.25))
}

/// Maps an SPDX id to the fit score's three licence classes.
fn license_class(spdx_id: Option<&str>) -> &'static str {
    let Some(id) = spdx_id else {
        return "unknown";
    };
    if yagna_core::health::PERMISSIVE_LICENSES.contains(&id) {
        return "permissive";
    }
    if id.contains("GPL") || id == "MPL-2.0" || id == "EPL-2.0" {
        return "copyleft";
    }
    "unknown"
}

/// Fact assembly against any connection (the re-score path's
/// transaction, or the store's own).
fn fit_facts_from_conn(
    conn: &rusqlite::Connection,
    repo_rowid: i64,
    now: DateTime<Utc>,
) -> Result<yagna_core::fit::FitFacts, StoreError> {
    let row = conn.query_row(FIT_FACTS_SELECT, params![repo_rowid], map_fit_facts_row)?;
    Ok(build_fit_facts(row, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::TagKind;
    use crate::testing::Scratch;
    use yagna_core::model::{Forge, LanguageStat, LicenseInfo};

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn snapshot(owner: &str, name: &str, stars: u64) -> RepoSnapshot {
        RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: owner.to_string(),
            name: name.to_string(),
            description: Some("a repo".to_string()),
            stars,
            languages: vec![LanguageStat {
                name: "Rust".to_string(),
                bytes: 1_000,
                color: Some("#dea584".to_string()),
            }],
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: vec!["cli".to_string()],
            default_branch: Some("main".to_string()),
            created_at: date(2020, 5, 1),
            last_commit_at: date(2026, 9, 1),
            starred_at: Some(date(2026, 10, 1)),
            commit_count: 500,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn row_count(store: &Store) -> u64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM repos", [], |row| row.get::<_, i64>(0))
            .expect("count") as u64
    }

    fn seen(snapshots: &[&RepoSnapshot]) -> Vec<(String, String)> {
        snapshots
            .iter()
            .map(|snapshot| (snapshot.owner.clone(), snapshot.name.clone()))
            .collect()
    }

    #[test]
    fn upsert_inserts_then_updates_preserving_first_seen() {
        let (_scratch, mut store) = store("upsert");
        let synced = date(2026, 10, 20);
        let written = store
            .upsert_snapshots(&[snapshot("someone", "thing", 10)], synced)
            .expect("insert");
        assert_eq!(written, 1);
        assert_eq!(row_count(&store), 1);

        let updated = date(2026, 10, 22);
        let written = store
            .upsert_snapshots(&[snapshot("someone", "thing", 11)], updated)
            .expect("update");
        assert_eq!(written, 1);
        assert_eq!(
            row_count(&store),
            1,
            "the identity key holds: no second row"
        );

        let (first_seen, stars, score, branch): (String, i64, i64, Option<String>) = store
            .conn
            .query_row(
                "SELECT first_seen_at, stars, health_score, default_branch
                 FROM repos WHERE owner = ?1 AND name = ?2",
                params!["someone", "thing"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("row");
        assert_eq!(first_seen, stamp(synced), "first sight survives the update");
        assert_eq!(stars, 11);
        assert!(score > 0, "health was scored, not defaulted");
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn upsert_writes_the_first_commit_default_on_insert_only() {
        let (_scratch, mut store) = store("first-commit");
        store
            .upsert_snapshots(&[snapshot("someone", "thing", 10)], date(2026, 10, 20))
            .expect("insert");
        let (first_date, source): (Option<String>, String) = store
            .conn
            .query_row(
                "SELECT first_commit_date, first_commit_source FROM repos WHERE name = ?1",
                params!["thing"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(first_date.as_deref(), Some("2020-05-01T12:00:00Z"));
        assert_eq!(source, "repo-created");

        store
            .upsert_snapshots(&[snapshot("someone", "thing", 11)], date(2026, 10, 21))
            .expect("re-upsert");
        let (first_date, source): (Option<String>, String) = store
            .conn
            .query_row(
                "SELECT first_commit_date, first_commit_source FROM repos WHERE name = ?1",
                params!["thing"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(first_date.as_deref(), Some("2020-05-01T12:00:00Z"));
        assert_eq!(source, "repo-created");
    }

    #[test]
    fn upsert_stores_the_health_json_and_the_json_columns() {
        let (_scratch, mut store) = store("json");
        store
            .upsert_snapshots(&[snapshot("someone", "thing", 10)], date(2026, 10, 20))
            .expect("insert");
        let (languages, topics): (String, String) = store
            .conn
            .query_row(
                "SELECT languages_json, topics_json FROM repos WHERE name = ?1",
                params!["thing"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        let languages: Vec<LanguageStat> = serde_json::from_str(&languages).expect("languages");
        assert_eq!(languages[0].name, "Rust");
        let topics: Vec<String> = serde_json::from_str(&topics).expect("topics");
        assert_eq!(topics, ["cli"]);
        let health = store.health_json_of("someone", "thing").expect("health");
        assert_eq!(health.license, 2, "MIT scores its two license points");
    }

    #[test]
    fn mark_missing_flags_only_rows_outside_the_seen_list() {
        let (_scratch, mut store) = store("missing");
        let kept = snapshot("someone", "kept", 10);
        let gone = snapshot("someone", "gone", 10);
        let first_sweep = date(2026, 10, 1);
        store
            .upsert_snapshots(&[kept.clone(), gone.clone()], first_sweep)
            .expect("insert both");
        let marked = store
            .mark_missing(&seen(&[&kept, &gone]), first_sweep)
            .expect("mark");
        assert_eq!(marked, 0, "both rows are in the seen list");

        // The next sweep sees only one of the two repositories.
        let second_sweep = date(2026, 10, 20);
        store
            .upsert_snapshots(std::slice::from_ref(&kept), second_sweep)
            .expect("upsert kept");
        let marked = store
            .mark_missing(&seen(&[&kept]), second_sweep)
            .expect("mark");
        assert_eq!(marked, 1, "exactly the unseen row goes missing");

        let (kept_missing, gone_missing): (Option<String>, Option<String>) = store
            .conn
            .query_row(
                "SELECT
                   (SELECT missing_at FROM repos WHERE name = 'kept'),
                   (SELECT missing_at FROM repos WHERE name = 'gone')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(kept_missing, None);
        assert_eq!(gone_missing, Some(stamp(second_sweep)));

        // A repository that comes back is cleared.
        store
            .upsert_snapshots(std::slice::from_ref(&gone), date(2026, 10, 25))
            .expect("the missing one returns");
        store
            .mark_missing(&seen(&[&gone]), date(2026, 10, 26))
            .expect("mark");
        let returned: Option<String> = store
            .conn
            .query_row(
                "SELECT missing_at FROM repos WHERE name = 'gone'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(returned, None, "a seen repository is not missing");
    }

    #[test]
    fn a_renamed_repository_leaves_two_rows_the_old_one_missing() {
        let (_scratch, mut store) = store("rename");
        let old = snapshot("someone", "old-name", 10);
        store
            .upsert_snapshots(std::slice::from_ref(&old), date(2026, 10, 1))
            .expect("insert");

        let new = snapshot("someone", "new-name", 10);
        store
            .upsert_snapshots(std::slice::from_ref(&new), date(2026, 10, 20))
            .expect("insert the renamed identity");
        let marked = store
            .mark_missing(&seen(&[&new]), date(2026, 10, 20))
            .expect("mark");
        assert_eq!(marked, 1);

        assert_eq!(
            row_count(&store),
            2,
            "renames are visible, not papered over"
        );
        let old_missing: Option<String> = store
            .conn
            .query_row(
                "SELECT missing_at FROM repos WHERE name = 'old-name'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert!(old_missing.is_some(), "the old identity went missing");
        let new_present: Option<String> = store
            .conn
            .query_row(
                "SELECT missing_at FROM repos WHERE name = 'new-name'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(new_present, None);
    }

    #[test]
    fn overview_counts_presence_shape_and_health() {
        let (_scratch, mut store) = store("overview");
        let a = snapshot("someone", "a", 10);
        let b = snapshot("someone", "b", 10);
        let mut forked = snapshot("someone", "fork", 10);
        forked.is_fork = true;
        store
            .upsert_snapshots(&[a, b, forked], date(2026, 10, 1))
            .expect("insert");
        store
            .mark_missing(&[], date(2026, 10, 2))
            .expect("mark all missing");

        let overview = store.overview().expect("overview");
        assert_eq!(overview.repos_total, 3);
        assert_eq!(overview.repos_missing, 3);
        assert_eq!(overview.repos_present, 0);
        assert_eq!(overview.forks, 1);
        assert_eq!(overview.health_unscored, 0);
    }

    #[test]
    fn count_first_seen_at_matches_one_sweeps_additions() {
        let (_scratch, mut store) = store("first-seen");
        let a = snapshot("someone", "a", 10);
        let first_sweep = date(2026, 10, 1);
        store
            .upsert_snapshots(std::slice::from_ref(&a), first_sweep)
            .expect("insert");
        let b = snapshot("someone", "b", 10);
        let second_sweep = date(2026, 10, 20);
        store
            .upsert_snapshots(&[a.clone(), b.clone()], second_sweep)
            .expect("upsert");
        assert_eq!(store.count_first_seen_at(first_sweep).expect("count"), 1);
        assert_eq!(
            store.count_first_seen_at(second_sweep).expect("count"),
            1,
            "the re-seen repository is not a new addition"
        );
    }

    fn rowid_of(store: &Store, owner: &str, name: &str) -> i64 {
        store
            .conn
            .query_row(
                "SELECT id FROM repos WHERE owner = ?1 AND name = ?2",
                params![owner, name],
                |row| row.get(0),
            )
            .expect("rowid")
    }

    /// Inserts a README excerpt for one repository, the way the
    /// enrichment layer would have left it.
    fn seed_excerpt(store: &Store, owner: &str, name: &str, excerpt: &str) {
        store
            .conn
            .execute(
                "INSERT OR REPLACE INTO readmes
                   (repo_rowid, original, rewritten, excerpt, truncated, fetched_at)
                 VALUES (?1, '', '', ?2, 0, '2026-10-01T00:00:00Z')",
                params![rowid_of(store, owner, name), excerpt],
            )
            .expect("readme insert");
    }

    /// Attaches tags to one repository through the tags tables the way
    /// the Part05 verbs will.
    fn seed_tags(store: &Store, owner: &str, name: &str, tags: &[&str]) {
        let rowid = rowid_of(store, owner, name);
        for tag in tags {
            store
                .conn
                .execute(
                    "INSERT OR IGNORE INTO tags (name, kind) VALUES (?1, 'tag')",
                    params![tag],
                )
                .expect("tag insert");
            let tag_id: i64 = store
                .conn
                .query_row("SELECT id FROM tags WHERE name = ?1", params![tag], |row| {
                    row.get(0)
                })
                .expect("tag id");
            store
                .conn
                .execute(
                    "INSERT OR IGNORE INTO repo_tags (repo_rowid, tag_id) VALUES (?1, ?2)",
                    params![rowid, tag_id],
                )
                .expect("attachment insert");
        }
    }

    fn page_names(page: &[RepoRow]) -> Vec<String> {
        page.iter()
            .map(|row| format!("{}/{}", row.repo_ref.owner, row.repo_ref.name))
            .collect()
    }

    #[test]
    fn the_page_and_the_detail_carry_the_fit_score() {
        use yagna_core::fit::default_spec;
        let (_scratch, mut store) = store("fit-page");
        store
            .upsert_snapshots(&[snapshot("someone", "a", 10)], date(2026, 10, 1))
            .expect("insert");
        store
            .rescore_fit(&default_spec(), date(2026, 10, 2))
            .expect("rescore");

        let rows = store
            .page(SortKey::Name, 0, 500, &CatalogFilter::empty())
            .expect("page");
        let fit_score = rows[0].fit_score.expect("the row carries its fit score");
        let detail = store
            .repo_detail(rows[0].repo_rowid as u64)
            .expect("detail")
            .expect("row");
        let fit = detail.fit.expect("the detail carries the fit breakdown");
        assert!(
            (fit.total - fit_score).abs() < 1e-9,
            "the two reads agree: {} vs {fit_score}",
            fit.total
        );
    }

    #[test]
    fn a_rescore_stamps_the_spec_version_that_scored_the_rows() {
        use yagna_core::fit::default_spec;
        let (_scratch, mut store) = store("fit-version");
        store
            .upsert_snapshots(&[snapshot("someone", "a", 10)], date(2026, 10, 1))
            .expect("insert");
        let rowid = rowid_of(&store, "someone", "a");
        let mut rewritten = default_spec();
        rewritten.version = 2;
        rewritten.name = "the rewrite".to_string();
        store
            .rescore_fit(&rewritten, date(2026, 10, 2))
            .expect("rescore");
        let (_total, json) = store
            .fit_score_of(rowid)
            .expect("scored")
            .expect("the row carries its breakdown");
        let breakdown: yagna_core::fit::FitReport =
            serde_json::from_str(&json).expect("the breakdown parses");
        assert_eq!(
            breakdown.version, 2,
            "a row scored by version 2 says version 2"
        );
    }

    #[test]
    fn invariant_a_rescore_never_moves_the_pinned_health_score() {
        use yagna_core::fit::default_spec;
        let (_scratch, mut store) = store("fit-untouched-health");
        store
            .upsert_snapshots(&[snapshot("someone", "a", 10)], date(2026, 10, 1))
            .expect("insert");
        let rowid = rowid_of(&store, "someone", "a");
        let before = store
            .repo_detail(rowid as u64)
            .expect("detail")
            .expect("row")
            .health;
        store
            .rescore_fit(&default_spec(), date(2026, 10, 2))
            .expect("rescore");
        let after = store
            .repo_detail(rowid as u64)
            .expect("detail")
            .expect("row")
            .health;
        assert_eq!(before, after, "the re-score touches the fit columns alone");
    }

    #[test]
    fn page_and_total_count_on_an_empty_catalog() {
        let (_scratch, store) = store("page-empty");
        let page = store
            .page(SortKey::StarredAt, 0, 500, &CatalogFilter::empty())
            .expect("page");
        assert!(page.is_empty());
        assert_eq!(
            store.total_count(&CatalogFilter::empty()).expect("total"),
            0
        );
    }

    #[test]
    fn page_orders_starred_at_newest_first_and_excludes_missing() {
        let (_scratch, mut store) = store("page-starred");
        let mut early = snapshot("someone", "early", 10);
        early.starred_at = Some(date(2026, 9, 1));
        let late = snapshot("someone", "late", 10);
        let mut mid = snapshot("someone", "mid", 10);
        mid.starred_at = Some(date(2026, 9, 15));
        let gone = snapshot("someone", "gone", 10);
        store
            .upsert_snapshots(&[early, mid, late, gone], date(2026, 10, 1))
            .expect("insert");
        // mark_missing flags everything outside the seen list — here
        // "late" (the newest starred) and "gone" both drop out.
        let seen_pairs: Vec<(String, String)> = [("someone", "early"), ("someone", "mid")]
            .iter()
            .map(|(owner, name)| (owner.to_string(), name.to_string()))
            .collect();
        let marked = store
            .mark_missing(&seen_pairs, date(2026, 10, 2))
            .expect("mark late missing");
        assert_eq!(marked, 2);

        let page = store
            .page(SortKey::StarredAt, 0, 500, &CatalogFilter::empty())
            .expect("page");
        assert_eq!(
            page_names(&page),
            ["someone/mid", "someone/early"],
            "newest starred_at first, the missing row excluded"
        );
        assert_eq!(
            store.total_count(&CatalogFilter::empty()).expect("total"),
            2
        );
    }

    #[test]
    fn page_tiebreak_orders_equal_keys_by_owner_then_name() {
        let (_scratch, mut store) = store("page-tiebreak");
        let beta = snapshot("beta", "same", 10);
        let zeta = snapshot("alpha", "zeta", 10);
        let beta_of_alpha = snapshot("alpha", "beta", 10);
        store
            .upsert_snapshots(&[beta, zeta, beta_of_alpha], date(2026, 10, 1))
            .expect("insert");

        let page = store
            .page(SortKey::Stars, 0, 500, &CatalogFilter::empty())
            .expect("page");
        assert_eq!(
            page_names(&page),
            ["alpha/beta", "alpha/zeta", "beta/same"],
            "equal stars break by (forge, owner, name)"
        );
    }

    #[test]
    fn page_offset_and_limit_walk_the_catalog_once() {
        let (_scratch, mut store) = store("page-offset");
        let seeded: Vec<RepoSnapshot> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|letter| snapshot("someone", letter, 10))
            .collect();
        store
            .upsert_snapshots(&seeded, date(2026, 10, 1))
            .expect("insert");

        let first = store
            .page(SortKey::Name, 0, 2, &CatalogFilter::empty())
            .expect("first page");
        let second = store
            .page(SortKey::Name, 2, 2, &CatalogFilter::empty())
            .expect("second page");
        let third = store
            .page(SortKey::Name, 4, 2, &CatalogFilter::empty())
            .expect("third page");
        let mut walked = Vec::new();
        walked.extend(page_names(&first));
        walked.extend(page_names(&second));
        walked.extend(page_names(&third));
        assert_eq!(
            walked,
            [
                "someone/a",
                "someone/b",
                "someone/c",
                "someone/d",
                "someone/e"
            ],
            "limit/offset pages tile the catalog without gaps or repeats"
        );
        assert_eq!(
            store.total_count(&CatalogFilter::empty()).expect("total"),
            5
        );
    }

    #[test]
    fn page_joins_excerpt_tags_and_health() {
        let (_scratch, mut store) = store("page-join");
        let plain = snapshot("someone", "plain", 10);
        let rich = snapshot("someone", "rich", 10);
        store
            .upsert_snapshots(&[plain, rich], date(2026, 10, 1))
            .expect("insert");
        seed_excerpt(&store, "someone", "rich", "the first line of the readme");
        seed_tags(&store, "someone", "rich", &["zeta", "Alpha"]);

        let page = store
            .page(SortKey::Name, 0, 500, &CatalogFilter::empty())
            .expect("page");
        assert_eq!(page.len(), 2);
        let rich_row = &page[1];
        assert_eq!(rich_row.repo_ref.name, "rich");
        assert_eq!(rich_row.excerpt, "the first line of the readme");
        assert_eq!(rich_row.tags, ["Alpha", "zeta"]);
        assert_eq!(rich_row.primary_language.as_deref(), Some("Rust"));
        assert_eq!(rich_row.license_name.as_deref(), Some("MIT License"));
        assert!(rich_row.health.total > 0, "health parsed from health_json");
        let plain_row = &page[0];
        assert_eq!(plain_row.excerpt, "", "no README means an empty excerpt");
        assert!(plain_row.tags.is_empty(), "no tags means an empty list");
    }

    #[test]
    fn page_matches_hand_written_sql_for_every_sort_key() {
        let (_scratch, mut store) = store("page-sql");
        let mut young = snapshot("someone", "young", 900);
        young.created_at = date(2025, 1, 1);
        young.last_commit_at = date(2026, 10, 15);
        let mut old = snapshot("someone", "old", 1_500);
        old.created_at = date(2012, 3, 4);
        old.last_commit_at = date(2026, 1, 1);
        let mut mid = snapshot("other", "mid", 1_200);
        mid.created_at = date(2020, 6, 6);
        mid.last_commit_at = date(2026, 5, 5);
        let missing = snapshot("other", "unseen", 2_000);
        store
            .upsert_snapshots(&[young, old, mid, missing], date(2026, 10, 1))
            .expect("insert");
        let missing_pairs: Vec<(String, String)> =
            [("someone", "young"), ("someone", "old"), ("other", "mid")]
                .iter()
                .map(|(owner, name)| (owner.to_string(), name.to_string()))
                .collect();
        store
            .mark_missing(&missing_pairs, date(2026, 10, 2))
            .expect("mark");

        for key in SortKey::ALL {
            let page = store
                .page(key, 0, 500, &CatalogFilter::empty())
                .expect("page");
            let sql = format!(
                "SELECT owner, name FROM repos WHERE missing_at IS NULL
                 ORDER BY {} LIMIT 500 OFFSET 0",
                key.order_by()
            );
            let expected: Vec<String> = store
                .conn
                .prepare(&sql)
                .expect("prepare")
                .query_map([], |row| {
                    Ok(format!(
                        "{}/{}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ))
                })
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("rows");
            assert_eq!(
                page_names(&page),
                expected,
                "{key:?} must order exactly like the hand-written SQL"
            );
        }
    }

    #[test]
    fn tag_names_lists_one_repositories_tags_sorted() {
        let (_scratch, mut store) = store("tag-names");
        let a = snapshot("someone", "a", 10);
        let b = snapshot("someone", "b", 10);
        store
            .upsert_snapshots(&[a, b], date(2026, 10, 1))
            .expect("insert");
        seed_tags(&store, "someone", "a", &["rust", "Books"]);

        assert_eq!(
            store
                .tag_names(rowid_of(&store, "someone", "a"))
                .expect("tags"),
            ["Books", "rust"],
            "case-insensitive sort"
        );
        assert!(
            store
                .tag_names(rowid_of(&store, "someone", "b"))
                .expect("tags")
                .is_empty()
        );
    }

    /// The id of the tag with that name, the way the verbs hand it out.
    fn tag_id_of(store: &Store, name: &str) -> i64 {
        store
            .conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .expect("tag id")
    }

    #[test]
    fn the_tag_filter_narrows_the_page_and_the_count() {
        let (_scratch, mut store) = store("filter-all");
        let a = snapshot("someone", "a", 10);
        let b = snapshot("someone", "b", 10);
        let c = snapshot("someone", "c", 10);
        store
            .upsert_snapshots(&[a, b, c], date(2026, 10, 1))
            .expect("insert");
        seed_tags(&store, "someone", "a", &["rust", "cli"]);
        seed_tags(&store, "someone", "b", &["rust"]);

        let both = CatalogFilter {
            tag_ids: vec![tag_id_of(&store, "rust"), tag_id_of(&store, "cli")],
            tag_match: TagMatch::All,
            ..CatalogFilter::empty()
        };
        let page = store.page(SortKey::Name, 0, 500, &both).expect("page");
        assert_eq!(page_names(&page), ["someone/a"], "every tag is required");
        assert_eq!(store.total_count(&both).expect("total"), 1);

        let either = CatalogFilter {
            tag_ids: vec![tag_id_of(&store, "rust"), tag_id_of(&store, "cli")],
            tag_match: TagMatch::Any,
            ..CatalogFilter::empty()
        };
        let page = store.page(SortKey::Name, 0, 500, &either).expect("page");
        assert_eq!(
            page_names(&page),
            ["someone/a", "someone/b"],
            "one tag is enough"
        );
        assert_eq!(store.total_count(&either).expect("total"), 2);

        let empty = CatalogFilter::empty();
        assert!(empty.is_empty());
        assert_eq!(store.total_count(&empty).expect("total"), 3);
    }

    #[test]
    fn the_candidate_ids_follow_the_sort_order() {
        let (_scratch, mut store) = store("candidates");
        let a = snapshot("someone", "a", 10);
        let mut b = snapshot("someone", "b", 30);
        b.stars = 30;
        store
            .upsert_snapshots(&[a, b], date(2026, 10, 1))
            .expect("insert");

        let ids = store
            .candidate_ids(SortKey::Stars, &CatalogFilter::empty())
            .expect("ids");
        assert_eq!(ids.len(), 2);
        let by_name = store
            .candidate_ids(SortKey::Name, &CatalogFilter::empty())
            .expect("ids");
        assert_eq!(
            by_name,
            vec![ids[1], ids[0]],
            "the name order differs from the star order"
        );
    }

    #[test]
    fn the_rows_by_ids_come_back_in_the_given_order() {
        let (_scratch, mut store) = store("rows-by-ids");
        let a = snapshot("someone", "a", 10);
        let b = snapshot("someone", "b", 20);
        let c = snapshot("someone", "c", 30);
        store
            .upsert_snapshots(&[a, b, c], date(2026, 10, 1))
            .expect("insert");
        let ids = store
            .candidate_ids(SortKey::Name, &CatalogFilter::empty())
            .expect("ids");

        let reversed = vec![ids[2], ids[0], ids[1]];
        let rows = store.rows_by_ids(&reversed).expect("rows");
        let names: Vec<String> = rows.iter().map(|row| row.repo_ref.name.clone()).collect();
        assert_eq!(names, ["c", "a", "b"], "the given order is kept");
        assert_eq!(rows[0].repo_rowid, ids[2], "the row id rides along");
    }

    #[test]
    fn the_search_index_rows_carry_the_haystack_fields() {
        let (_scratch, mut store) = store("search-rows");
        let a = snapshot("someone", "a", 10);
        let b = snapshot("someone", "b", 10);
        store
            .upsert_snapshots(&[a, b], date(2026, 10, 1))
            .expect("insert");
        seed_tags(&store, "someone", "a", &["reading list"]);

        let rows = store.search_index_rows().expect("rows");
        assert_eq!(rows.len(), 2);
        let first = rows
            .iter()
            .find(|row| row.name == "a")
            .expect("the tagged repository");
        assert_eq!(first.owner, "someone");
        assert_eq!(first.description.as_deref(), Some("a repo"));
        assert_eq!(first.topics, ["cli"]);
        assert_eq!(first.tags, ["reading list"]);

        let one = store
            .search_index_row(first.repo_rowid)
            .expect("row")
            .expect("a row");
        assert_eq!(one.name, "a");
        assert!(
            store.search_index_row(9999).expect("row").is_none(),
            "an unknown row has no search row"
        );
    }

    /// The topic half of the filter narrows the page, the total and the
    /// candidate set the same way the tag half does, and combines with
    /// it. A topic names a whole element of the repository's own list —
    /// "cli" is not "cli-tools".
    #[test]
    fn a_topic_narrows_the_page_the_total_and_the_candidates() {
        let (_scratch, mut store) = store("topic-filter");
        let mut themed = snapshot("someone", "themed", 10);
        themed.topics = vec!["rust".to_string(), "cli".to_string()];
        let mut plain = snapshot("someone", "plain", 10);
        plain.topics = vec!["cli-tools".to_string()];
        let mut tagged = snapshot("someone", "tagged", 10);
        tagged.topics = vec!["rust".to_string()];
        store
            .upsert_snapshots(&[themed, plain, tagged], date(2026, 10, 1))
            .expect("insert");
        let tag = store
            .assign_tag(2, "reading", Some(TagKind::Tag))
            .expect("tag");

        let topic = CatalogFilter {
            topic: Some("rust".to_string()),
            ..CatalogFilter::empty()
        };
        assert_eq!(store.total_count(&topic).expect("total"), 2);
        assert_eq!(
            store
                .page(SortKey::Stars, 0, 10, &topic)
                .expect("page")
                .len(),
            2
        );
        assert_eq!(
            store
                .candidate_ids(SortKey::Stars, &topic)
                .expect("ids")
                .len(),
            2
        );

        // "cli" matches the whole element; "cli-tools" is another topic.
        let cli = CatalogFilter {
            topic: Some("cli".to_string()),
            ..CatalogFilter::empty()
        };
        assert_eq!(store.total_count(&cli).expect("total"), 1);

        // The two halves combine: the tag and the topic together.
        let both = CatalogFilter {
            tag_ids: vec![tag],
            tag_match: TagMatch::All,
            topic: Some("cli".to_string()),
        };
        assert_eq!(
            store.total_count(&both).expect("total"),
            0,
            "the tagged repository does not carry the cli topic"
        );
        assert!(!both.is_empty());
        assert!(CatalogFilter::empty().is_empty());
    }

    /// The topic counts aggregate over the filter's tag half — the
    /// selected topic keeps its true count while the others show what
    /// the tag filter leaves them.
    #[test]
    fn the_topic_counts_aggregate_over_the_tag_half() {
        let (_scratch, mut store) = store("topic-counts");
        let mut themed = snapshot("someone", "themed", 10);
        themed.topics = vec!["rust".to_string(), "cli".to_string()];
        let mut other = snapshot("someone", "other", 10);
        other.topics = vec!["rust".to_string()];
        store
            .upsert_snapshots(&[themed, other], date(2026, 10, 1))
            .expect("insert");

        let counts = topic_counts(&store, &CatalogFilter::empty()).expect("counts");
        assert_eq!(counts.len(), 2);
        assert_eq!(
            counts[0],
            NameCount {
                name: "rust".to_string(),
                count: 2
            }
        );
        assert_eq!(
            counts[1],
            NameCount {
                name: "cli".to_string(),
                count: 1
            }
        );

        // A selected topic does not empty the others' counts.
        let with_topic = CatalogFilter {
            topic: Some("rust".to_string()),
            ..CatalogFilter::empty()
        };
        let counts = topic_counts(&store, &with_topic).expect("counts");
        assert_eq!(counts[0].name, "rust");
        assert_eq!(counts[0].count, 2, "the selected topic keeps its count");
    }
}
