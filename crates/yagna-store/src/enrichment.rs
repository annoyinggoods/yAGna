//! The lazy layer: README/license/tree storage and the enrichment
//! queue its serial runner works.
//!
//! Storage is the pipeline's landing: `store_readme` runs the core's
//! pure markdown math (cap → rewrite → excerpt), writes the readmes row
//! and seeds the asset inventory — one row per unique URL, `refcount`
//! counting the referencing repositories. The queue is resume-by-state:
//! `enqueue_missing` fills gaps idempotently (only repos lacking the
//! kind, and only without a live row already), `claim_pending` marks the
//! oldest eligible row `claimed` under a lease, and the stamps move it
//! to `done`, `skipped` (a recorded reason), back to `pending` after a
//! backoff, or `failed` after three attempts. A claimed row whose lease
//! lapsed (its runner died) is claimable again — a killed run loses
//! nothing; a re-run continues at the head. A `not_before` stamp hides a
//! row from claims until it passes: the queue's pause on rate limits
//! and failed attempts.
//!
//! Items: `QueueKind` — readme | license | tree | digest (digest is
//! demand-only and refused by the bulk fill); `QueueItem` — one
//! claimed row with the repository identity it needs;
//! `QueueDepths` — the queue's state counts; `README_MAX_BYTES` — the
//! README cap; `LEASE_SECS`/`RETRY_BACKOFF_SECS` — the claim's lease
//! and the failed attempt's backoff; `store_readme`/`store_license_text`/
//! `store_tree` — the storage verbs; `enqueue_missing`/`claim_pending`/
//! `heartbeat`/the stamps — the queue's half.
//! Depends on: rusqlite, chrono, yagna_core::markdown (the rewrite
//! pipeline), yagna_core::treefmt (the render), crate::catalog (the
//! stamp format).
//! Called by: crate::lib (the Store facade) and the app's enrich
//! runner.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;
use yagna_core::markdown::{RawBase, readme_excerpt, rewrite_assets};
use yagna_core::model::TreeEntry;
use yagna_core::treefmt;

use crate::{Store, StoreError, catalog, saturating_rowid};

/// The README size cap: past this the stored original stops (on a character
/// boundary), `truncated` is stamped, and the excerpt notes it. The two
/// italic list lines never needed more anyway.
pub const README_MAX_BYTES: usize = 256 * 1024;

/// The attempts a queue row gets before it fails for real.
pub const MAX_ATTEMPTS: u64 = 3;

/// How long a claim holds without a heartbeat: the lease the runner
/// renews while its job runs. A row whose lease lapsed was abandoned —
/// its runner is dead and the job is claimable again.
pub const LEASE_SECS: i64 = 600;

/// The backoff a failed attempt waits before the row is claimable
/// again: the attempt counter waits, it does not spin.
pub const RETRY_BACKOFF_SECS: i64 = 30;

/// The claim's lease length.
pub(crate) fn lease_duration() -> chrono::Duration {
    chrono::Duration::seconds(LEASE_SECS)
}

/// The failed attempt's backoff length.
pub(crate) fn retry_backoff() -> chrono::Duration {
    chrono::Duration::seconds(RETRY_BACKOFF_SECS)
}

/// What a queue row works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Readme,
    License,
    Tree,
    /// The digest: demand-only. The bulk `enqueue_missing` refuses it —
    /// a digest is taken when a reader asks for one, never for the
    /// catalog.
    Digest,
}

impl QueueKind {
    /// The stored kind value.
    pub fn as_str(self) -> &'static str {
        match self {
            QueueKind::Readme => "readme",
            QueueKind::License => "license",
            QueueKind::Tree => "tree",
            QueueKind::Digest => "digest",
        }
    }

    /// The stored kind value back into a kind; unknown text is none.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "readme" => Some(QueueKind::Readme),
            "license" => Some(QueueKind::License),
            "tree" => Some(QueueKind::Tree),
            "digest" => Some(QueueKind::Digest),
            _ => None,
        }
    }

    /// The table whose presence marks the kind as satisfied.
    fn satisfied_by(self) -> &'static str {
        match self {
            QueueKind::Readme => "readmes",
            QueueKind::License => "license_texts",
            QueueKind::Tree => "trees",
            QueueKind::Digest => "digests",
        }
    }
}

/// One claimed queue row, carrying the repository identity the fetch
/// needs: who to ask, and the branch (when there is one) to ask about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub repo_rowid: u64,
    pub kind: QueueKind,
    pub owner: String,
    pub name: String,
    pub default_branch: Option<String>,
    /// The claim's ownership token: heartbeats and stamps must carry it
    /// back, so a runner whose lease lapsed (and whose row was
    /// re-claimed) cannot act on a row that is no longer its own.
    pub claim_token: String,
}

/// The queue's state counts, one query for the summary lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueDepths {
    pub pending: u64,
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl Store {
    // --- Storage: the pipeline's landing side.

    /// Stores one repository's README through the pipeline: the 256 KB
    /// cap, the deterministic asset rewrite (against the repository's
    /// raw base — a repository with no default branch resolves against
    /// `HEAD`), the excerpt, the row, and the asset inventory every
    /// found URL is upserted into. One transaction.
    pub fn store_readme(
        &mut self,
        repo_rowid: u64,
        original: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let (original, truncated) = cap_readme(original);
        let (owner, name) = self.repo_identity(repo_rowid)?;
        let base = RawBase {
            owner,
            repo: name,
            branch: self
                .repo_branch(repo_rowid)?
                .unwrap_or_else(|| "HEAD".to_string()),
        };
        let (rewritten, assets) = rewrite_assets(original, &base);
        let mut excerpt = readme_excerpt(original);
        if truncated {
            let stem = excerpt.trim_end_matches('…').trim_end();
            excerpt = format!("{stem} …(truncated)");
        }
        let transaction = self.conn.transaction()?;
        // The assets this README pointed at before the replace — the
        // refcount repair below needs the dropped ones too, or an asset
        // a re-fetched README no longer mentions keeps its old count
        // forever.
        let previous_assets: Vec<String> = {
            let mut statement =
                transaction.prepare("SELECT asset_id FROM readme_assets WHERE repo_rowid = ?1")?;
            let rows = statement.query_map(params![saturating_rowid(repo_rowid)], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<_, _>>()?
        };
        // The external-content index is kept in step here, inside the
        // body's own transaction: the old row's value is what the index
        // deletes with, the new one is what it re-inserts.
        transaction.execute(
            "INSERT INTO fts_readme(fts_readme, rowid, original)
             SELECT 'delete', repo_rowid, original FROM readmes WHERE repo_rowid = ?1",
            params![saturating_rowid(repo_rowid)],
        )?;
        transaction.execute(
            "INSERT OR REPLACE INTO readmes
               (repo_rowid, original, rewritten, excerpt, truncated, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                saturating_rowid(repo_rowid),
                original,
                rewritten,
                excerpt,
                i64::from(truncated),
                catalog::stamp(now),
            ],
        )?;
        transaction.execute(
            "INSERT INTO fts_readme(rowid, original) VALUES (?1, ?2)",
            params![saturating_rowid(repo_rowid), original],
        )?;
        // The mapping table is the indexable answer to "which assets
        // does this README mention" — rebuilt for this repository in
        // the same transaction, first-appearance order and all.
        transaction.execute(
            "DELETE FROM readme_assets WHERE repo_rowid = ?1",
            params![saturating_rowid(repo_rowid)],
        )?;
        for asset in &assets {
            transaction.execute(
                "INSERT INTO assets (asset_id, url, badge, state)
                 VALUES (?1, ?2, ?3, 'missing')
                 ON CONFLICT(asset_id) DO UPDATE SET badge = badge | excluded.badge",
                params![
                    asset.image.asset_id,
                    asset.image.url,
                    i64::from(asset.badge),
                ],
            )?;
        }
        for (position, asset) in assets.iter().enumerate() {
            transaction.execute(
                "INSERT INTO readme_assets (repo_rowid, asset_id, position) VALUES (?1, ?2, ?3)",
                params![
                    saturating_rowid(repo_rowid),
                    asset.image.asset_id,
                    position as i64,
                ],
            )?;
        }
        // Refcounts over the mapping table, for every asset this
        // repository mentioned before or mentions now — the dropped
        // ones are where the old drift lived.
        let mut touched = previous_assets;
        touched.extend(assets.iter().map(|asset| asset.image.asset_id.clone()));
        for asset_id in &touched {
            transaction.execute(
                "UPDATE assets SET refcount = (
                   SELECT COUNT(*) FROM readme_assets ra WHERE ra.asset_id = ?1
                 ) WHERE asset_id = ?1",
                params![asset_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Stores one repository's license file text — the non-standard
    /// licenses only; standard SPDX rows never reach the queue.
    pub fn store_license_text(
        &mut self,
        repo_rowid: u64,
        body: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO license_texts (repo_rowid, body, fetched_at)
             VALUES (?1, ?2, ?3)",
            params![saturating_rowid(repo_rowid), body, catalog::stamp(now)],
        )?;
        Ok(())
    }

    /// Stores one repository's tree: rendered through the core's pure
    /// tree format at storage time, truncation flag kept beside it.
    pub fn store_tree(
        &mut self,
        repo_rowid: u64,
        entries: &[TreeEntry],
        truncated: bool,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trees (repo_rowid, rendered, truncated, fetched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                saturating_rowid(repo_rowid),
                treefmt::render(entries, truncated),
                i64::from(truncated),
                catalog::stamp(now),
            ],
        )?;
        Ok(())
    }

    // --- The queue: enqueue, claim, stamp.

    /// Fills the queue for every repository lacking `kind` — idempotent:
    /// a repository with the kind's row satisfied, or with a live
    /// (pending/claimed/done/skipped) queue row, is not enqueued again.
    /// Failed rows may re-enqueue: the next run is their retry. The
    /// license kind enqueues only repositories with no standard SPDX
    /// id — the API's null SPDX id, and its "NOASSERTION" (which says
    /// *there is a license, and it is not one of ours*), are the
    /// non-standard signals.
    pub fn enqueue_missing(
        &mut self,
        kind: QueueKind,
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        if kind == QueueKind::Digest {
            return Err(StoreError::DigestOnDemand);
        }
        let license_only = " AND (license_spdx IS NULL OR license_spdx = 'NOASSERTION')";
        let sql = format!(
            "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
             SELECT id, '{kind}', ?1 FROM repos
             WHERE NOT EXISTS (
               SELECT 1 FROM {table} WHERE {table}.repo_rowid = repos.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM enrichment_queue
               WHERE enrichment_queue.repo_rowid = repos.id
                 AND enrichment_queue.kind = '{kind}'
                 AND enrichment_queue.state IN ('pending', 'claimed', 'done', 'skipped')
             ){license_gate}",
            kind = kind.as_str(),
            table = kind.satisfied_by(),
            license_gate = if kind == QueueKind::License {
                license_only
            } else {
                ""
            },
        );
        let inserted = self.conn.execute(&sql, params![catalog::stamp(now)])?;
        Ok(inserted as u64)
    }

    /// Claims the oldest eligible row — with the repository identity
    /// the fetch needs. A claim is a lease: the row is stamped
    /// `claimed` with a `lease_until` deadline, and a claim whose lease
    /// lapsed (its runner died mid-job) is claimable again.
    pub fn claim_pending(&self) -> Result<Option<QueueItem>, StoreError> {
        self.claim_where("q.repo_rowid = q.repo_rowid", &[])
    }

    /// Claims the oldest eligible row of one kind for one repository —
    /// the tab's "load structure" action works its own row and nobody
    /// else's.
    pub fn claim_pending_for(
        &self,
        repo_rowid: u64,
        kind: QueueKind,
    ) -> Result<Option<QueueItem>, StoreError> {
        self.claim_where(
            "q.repo_rowid = ?1 AND q.kind = ?2",
            &[
                rusqlite::types::Value::Integer(saturating_rowid(repo_rowid)),
                rusqlite::types::Value::Text(kind.as_str().to_string()),
            ],
        )
    }

    /// Renews a claimed row's lease — the runner's heartbeat, so a
    /// genuinely slow job (a digest's tarball) keeps its claim while it
    /// works. Only the token's own claim is renewed: `false` means the
    /// lease lapsed and another runner took the row — the caller lets
    /// go instead of working a row that is no longer its own.
    pub fn heartbeat(&self, id: u64, token: &str, now: DateTime<Utc>) -> Result<bool, StoreError> {
        let renewed = self.conn.execute(
            "UPDATE enrichment_queue SET lease_until = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2 AND state = 'claimed'",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(now + lease_duration()),
                catalog::stamp(now),
            ],
        )?;
        Ok(renewed > 0)
    }

    /// Puts a claimed row back without burning an attempt: `pending`
    /// again, lease cleared, and the retry backoff stamped so the row
    /// is not claimed in a hot loop. The transport-failure path's
    /// release; a token that no longer owns the row releases nothing.
    pub fn release_claim(
        &mut self,
        id: u64,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE enrichment_queue
             SET state = 'pending', lease_until = NULL, claim_token = NULL,
                 not_before = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2 AND state = 'claimed'",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(now + retry_backoff()),
                catalog::stamp(now),
            ],
        )?;
        Ok(())
    }

    /// Parks a rate-limited row: back to `pending` (no attempt burned),
    /// invisible to claims until `until` passes — the quota's reset.
    /// A job parked past its stall cap is simply not seen again this
    /// run; the next run claims it as usual.
    pub fn stamp_rate_limited(
        &mut self,
        id: u64,
        token: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE enrichment_queue
             SET state = 'pending', lease_until = NULL, claim_token = NULL,
                 not_before = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(until),
                catalog::stamp(now)
            ],
        )?;
        Ok(())
    }

    /// The earliest `not_before` among the queue's waiting rows — the
    /// "waiting until" the runner's visible pause counts down to.
    pub fn backoff_until(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let text: Option<String> = self.conn.query_row(
            "SELECT MIN(not_before) FROM enrichment_queue
              WHERE not_before IS NOT NULL AND state = 'pending'",
            [],
            |row| row.get(0),
        )?;
        match text {
            Some(text) => Ok(Some(catalog::parse_stamp(&text)?)),
            None => Ok(None),
        }
    }

    /// The claim itself: inside one immediate transaction, the oldest
    /// eligible row behind `clause` — `pending`, or `claimed` with a
    /// lapsed lease, and past its `not_before` if it carries one — is
    /// stamped `claimed` with a fresh lease and a fresh ownership token
    /// (the database's own randomblob — no crate gains an RNG for
    /// this), and read back with its identity. Immediate, so two
    /// processes against one database serialize here instead of
    /// failing at the write upgrade.
    fn claim_where(
        &self,
        clause: &str,
        values: &[rusqlite::types::Value],
    ) -> Result<Option<QueueItem>, StoreError> {
        let now = Utc::now();
        let transaction = crate::ImmediateTx::begin(&self.conn)?;
        let mut bound: Vec<rusqlite::types::Value> = values.to_vec();
        let now_index = bound.len() + 1;
        bound.push(rusqlite::types::Value::Text(catalog::stamp(now)));
        let eligibility = format!(
            "SELECT q.id FROM enrichment_queue q
              WHERE (q.state = 'pending'
                     OR (q.state = 'claimed'
                         AND (q.lease_until IS NULL OR q.lease_until <= ?{now_index})))
                AND (q.not_before IS NULL OR q.not_before <= ?{now_index})
                AND {clause}
              ORDER BY q.id LIMIT 1"
        );
        let id: Option<i64> = transaction
            .conn
            .query_row(
                &eligibility,
                rusqlite::params_from_iter(bound.iter()),
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        transaction.conn.execute(
            "UPDATE enrichment_queue
             SET state = 'claimed', lease_until = ?2, updated_at = ?3,
                 claim_token = lower(hex(randomblob(16)))
              WHERE id = ?1",
            params![
                id,
                catalog::stamp(now + lease_duration()),
                catalog::stamp(now)
            ],
        )?;
        let item = read_item(transaction.conn, id)?;
        transaction.commit()?;
        Ok(Some(item))
    }

    /// Stamps a claimed row done. A token that no longer owns the row
    /// stamps nothing — the row belongs to another runner.
    pub fn stamp_done(
        &mut self,
        id: u64,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE enrichment_queue
             SET state = 'done', last_error = NULL, lease_until = NULL, not_before = NULL,
                 claim_token = NULL, updated_at = ?3
              WHERE id = ?1 AND claim_token = ?2",
            params![saturating_rowid(id), token, catalog::stamp(now)],
        )?;
        Ok(())
    }

    /// Stamps a claimed row skipped, with the reason recorded — no
    /// README exists, the tree 404s. Token-guarded like every stamp.
    pub fn stamp_skipped(
        &mut self,
        id: u64,
        token: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE enrichment_queue
             SET state = 'skipped', last_error = ?3, lease_until = NULL, not_before = NULL,
                 claim_token = NULL, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2",
            params![saturating_rowid(id), token, reason, catalog::stamp(now)],
        )?;
        Ok(())
    }

    /// Counts one failed attempt. From [`MAX_ATTEMPTS`] on, the row
    /// fails for real; before that it returns to `pending` under the
    /// retry backoff — the attempt counter waits. Token-guarded like
    /// every stamp.
    pub fn stamp_attempt_failed(
        &mut self,
        id: u64,
        token: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE enrichment_queue
             SET attempts = attempts + 1,
                 state = CASE WHEN attempts + 1 >= ?3 THEN 'failed' ELSE 'pending' END,
                 lease_until = NULL,
                 claim_token = NULL,
                 not_before = CASE WHEN attempts + 1 < ?3 THEN ?6 ELSE NULL END,
                 last_error = ?4, updated_at = ?5
             WHERE id = ?1 AND claim_token = ?2",
            params![
                saturating_rowid(id),
                token,
                MAX_ATTEMPTS as i64,
                error,
                catalog::stamp(now),
                catalog::stamp(now + retry_backoff()),
            ],
        )?;
        Ok(())
    }

    /// How many rows sit in each state — one query, the summary's input.
    /// A `claimed` row counts as pending: it is work not yet finished.
    pub fn queue_depths(&self) -> Result<QueueDepths, StoreError> {
        self.conn
            .query_row(
                "SELECT
                   COUNT(*) FILTER (WHERE state IN ('pending', 'claimed')),
                   COUNT(*) FILTER (WHERE state = 'done'),
                   COUNT(*) FILTER (WHERE state = 'failed'),
                   COUNT(*) FILTER (WHERE state = 'skipped')
                 FROM enrichment_queue",
                [],
                |row| {
                    Ok(QueueDepths {
                        pending: row.get::<_, i64>(0)?.max(0) as u64,
                        done: row.get::<_, i64>(1)?.max(0) as u64,
                        failed: row.get::<_, i64>(2)?.max(0) as u64,
                        skipped: row.get::<_, i64>(3)?.max(0) as u64,
                    })
                },
            )
            .map_err(StoreError::from)
    }

    /// How many rows of one kind still wait — the progress line's total.
    pub fn count_pending(&self, kind: QueueKind) -> Result<u64, StoreError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM enrichment_queue
              WHERE kind = ?1 AND state IN ('pending', 'claimed')",
            params![kind.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    // --- Private lookups the storage verbs lean on.

    /// The repository's (owner, name) — the raw base's first half.
    fn repo_identity(&self, repo_rowid: u64) -> Result<(String, String), StoreError> {
        self.conn
            .query_row(
                "SELECT owner, name FROM repos WHERE id = ?1",
                params![saturating_rowid(repo_rowid)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::from)
    }

    /// The repository's default branch, when it has one.
    fn repo_branch(&self, repo_rowid: u64) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT default_branch FROM repos WHERE id = ?1",
                params![saturating_rowid(repo_rowid)],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }
}

/// The README cap applied: text back (cut on a character boundary when
/// the cap lands mid-character) and whether it was cut.
fn cap_readme(original: &str) -> (&str, bool) {
    if original.len() <= README_MAX_BYTES {
        return (original, false);
    }
    let mut cut = README_MAX_BYTES;
    while cut > 0 && !original.is_char_boundary(cut) {
        cut -= 1;
    }
    (&original[..cut], true)
}

/// One queue row's data by id — the row a claim just stamped, read back
/// with the repository identity its fetch needs.
fn read_item(connection: &rusqlite::Connection, id: i64) -> Result<QueueItem, StoreError> {
    let mut statement = connection.prepare(
        "SELECT q.id, q.repo_rowid, q.kind, r.owner, r.name, r.default_branch, q.claim_token
         FROM enrichment_queue q JOIN repos r ON r.id = q.repo_rowid
         WHERE q.id = ?1",
    )?;
    let mut rows = statement.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Err(parse_error(format!("queue row {id} vanished mid-claim")));
    };
    let kind_text: String = row.get(2)?;
    let Some(kind) = QueueKind::parse(&kind_text) else {
        return Err(parse_error(format!(
            "queue row holds an unknown kind: {kind_text:?}"
        )));
    };
    Ok(QueueItem {
        id: row.get::<_, i64>(0)?.max(0) as u64,
        repo_rowid: row.get::<_, i64>(1)?.max(0) as u64,
        kind,
        owner: row.get(3)?,
        name: row.get(4)?,
        default_branch: row.get(5)?,
        claim_token: row.get(6)?,
    })
}

/// The store-side parse error, for data that should not exist.
fn parse_error(message: String) -> StoreError {
    StoreError::Parse(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scratch, sample_snapshot};
    use yagna_core::model::{RepoSnapshot, TreeEntryKind};

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

    fn now() -> DateTime<Utc> {
        date(2026, 10, 27)
    }

    fn seed(store: &mut Store, owners_names: &[(&str, &str)]) {
        let snapshots: Vec<RepoSnapshot> = owners_names
            .iter()
            .map(|(owner, name)| sample_snapshot(owner, name))
            .collect();
        store
            .upsert_snapshots(&snapshots, date(2026, 10, 20))
            .expect("seed");
    }

    fn rowids(store: &Store) -> Vec<u64> {
        let mut statement = store
            .conn
            .prepare("SELECT id FROM repos ORDER BY id")
            .expect("prepare");
        let mut rows = statement.query([]).expect("query");
        let mut ids = Vec::new();
        while let Some(row) = rows.next().expect("row") {
            ids.push(row.get::<_, i64>(0).expect("id") as u64);
        }
        ids
    }

    fn state_of(store: &Store, id: u64) -> String {
        store
            .conn
            .query_row(
                "SELECT state FROM enrichment_queue WHERE id = ?1",
                params![id as i64],
                |row| row.get(0),
            )
            .expect("state")
    }

    #[test]
    fn readme_storage_rewrites_seeds_assets_and_counts_references() {
        let (_scratch, mut store) = store("readme-store");
        seed(
            &mut store,
            &[("someone", "with-readme"), ("someone", "sharer")],
        );
        let ids = rowids(&store);

        let md = "# Demo\n\n[![ci](https://img.shields.io/badge/ci-ok)](https://ci)\n\n![shot](docs/shot.png) and text.\n";
        store.store_readme(ids[0], md, now()).expect("store");

        let (original, rewritten, excerpt, truncated): (String, String, String, i64) = store
            .conn
            .query_row(
                "SELECT original, rewritten, excerpt, truncated FROM readmes WHERE repo_rowid = ?1",
                params![ids[0] as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("readmes row");
        assert_eq!(original, md);
        assert!(!rewritten.contains("docs/shot.png"));
        assert!(rewritten.contains("yagna-asset://"));
        assert_eq!(truncated, 0);
        assert!(
            excerpt.contains("and text"),
            "the excerpt is text: {excerpt}"
        );
        assert!(!excerpt.contains("ci-ok"), "no badge alt text");

        let (badge_count, asset_count): (i64, i64) = store
            .conn
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM assets WHERE badge = 1),
                   (SELECT COUNT(*) FROM assets)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("assets");
        assert_eq!(
            asset_count, 2,
            "the badge and the screenshot are one row each"
        );
        assert_eq!(badge_count, 1, "the shields URL is marked");
        let refcount: i64 = store
            .conn
            .query_row("SELECT refcount FROM assets WHERE badge = 0", [], |row| {
                row.get(0)
            })
            .expect("refcount");
        assert_eq!(refcount, 1);

        // The same screenshot in a second README: one asset, refcount 2.
        let shot_url = store
            .conn
            .query_row("SELECT url FROM assets WHERE badge = 0", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("url");
        let shared = format!("# Two\n\n![again]({shot_url})\n");
        store.store_readme(ids[1], &shared, now()).expect("store 2");
        let refcount: i64 = store
            .conn
            .query_row("SELECT refcount FROM assets WHERE badge = 0", [], |row| {
                row.get(0)
            })
            .expect("refcount");
        assert_eq!(refcount, 2, "two repositories reference it");
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM assets", [], |row| row
                    .get::<_, i64>(0))
                .expect("count"),
            2,
            "still one row per URL"
        );
    }

    #[test]
    fn readme_storage_caps_and_notes_truncation() {
        let (_scratch, mut store) = store("readme-cap");
        seed(&mut store, &[("someone", "huge")]);
        let id = rowids(&store).remove(0);

        let huge = "word ".repeat(README_MAX_BYTES / 5 + 40);
        store.store_readme(id, &huge, now()).expect("store");
        let (stored_len, truncated, excerpt): (i64, i64, String) = store
            .conn
            .query_row(
                "SELECT length(original), truncated, excerpt FROM readmes WHERE repo_rowid = ?1",
                params![id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("row");
        assert!(stored_len as usize <= README_MAX_BYTES);
        assert_eq!(truncated, 1);
        assert!(
            excerpt.contains("truncated"),
            "the excerpt notes it: {excerpt}"
        );
    }

    #[test]
    fn license_and_tree_storage_roundtrip() {
        let (_scratch, mut store) = store("license-tree");
        seed(&mut store, &[("someone", "full")]);
        let id = rowids(&store).remove(0);

        store
            .store_license_text(id, "Hippocratic License 2.1 text…", now())
            .expect("license");
        let body: String = store
            .conn
            .query_row(
                "SELECT body FROM license_texts WHERE repo_rowid = ?1",
                params![id as i64],
                |row| row.get(0),
            )
            .expect("body");
        assert!(body.starts_with("Hippocratic"));

        let entries = vec![
            TreeEntry {
                path: "src".to_string(),
                kind: TreeEntryKind::Tree,
                size: None,
            },
            TreeEntry {
                path: "src/main.rs".to_string(),
                kind: TreeEntryKind::Blob,
                size: Some(12),
            },
        ];
        store.store_tree(id, &entries, true, now()).expect("tree");
        let (rendered, truncated): (String, i64) = store
            .conn
            .query_row(
                "SELECT rendered, truncated FROM trees WHERE repo_rowid = ?1",
                params![id as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("tree row");
        assert!(rendered.contains("main.rs"), "the tree renders: {rendered}");
        assert!(
            rendered.starts_with("└── src\n"),
            "src is the single root, so last"
        );
        assert!(rendered.ends_with("… (structure truncated by the forge)\n"));
        assert_eq!(truncated, 1);
    }

    #[test]
    fn enqueue_fills_only_the_lacking_and_stays_idempotent() {
        let (_scratch, mut store) = store("enqueue");
        seed(&mut store, &[("someone", "a"), ("someone", "b")]);
        let ids = rowids(&store);

        let queued = store
            .enqueue_missing(QueueKind::Readme, now())
            .expect("enqueue");
        assert_eq!(queued, 2, "both repos lack readmes");
        assert_eq!(
            store
                .enqueue_missing(QueueKind::Readme, now())
                .expect("again"),
            0,
            "pending rows are not duplicated"
        );

        let claimed = store.claim_pending().expect("claim").expect("an item");
        store
            .stamp_done(claimed.id, &claimed.claim_token, now())
            .expect("done");
        assert_eq!(
            store
                .enqueue_missing(QueueKind::Readme, now())
                .expect("again"),
            0,
            "a satisfied repo is not re-enqueued"
        );

        // A failed row gets a second chance on the next run: three
        // claim-and-fail cycles, the backoff cleared between them the
        // way time passing clears it.
        for _ in 0..3 {
            let item = store.claim_pending().expect("claim").expect("an item");
            store
                .stamp_attempt_failed(item.id, &item.claim_token, "boom", now())
                .expect("attempt");
            store
                .conn
                .execute(
                    "UPDATE enrichment_queue SET not_before = NULL WHERE id = ?1",
                    params![item.id as i64],
                )
                .expect("the backoff is over");
        }
        assert_eq!(state_of(&store, ids[1]), "failed", "three strikes");
        assert_eq!(
            store
                .enqueue_missing(QueueKind::Readme, now())
                .expect("again"),
            1,
            "the failed row re-enqueues"
        );
    }

    #[test]
    fn license_enqueue_targets_only_null_spdx_repos() {
        let (_scratch, mut store) = store("license-trigger");
        seed(
            &mut store,
            &[("someone", "licensed"), ("someone", "unlicensed")],
        );
        let ids = rowids(&store);
        // The sample snapshot carries MIT; make the second row non-standard.
        store
            .conn
            .execute(
                "UPDATE repos SET license_spdx = NULL, license_name = 'Custom 1.0' WHERE id = ?1",
                params![ids[1] as i64],
            )
            .expect("null the spdx");

        let queued = store
            .enqueue_missing(QueueKind::License, now())
            .expect("enqueue");
        assert_eq!(queued, 1, "only the null-SPDX row is a candidate");
        let rowid: i64 = store
            .conn
            .query_row(
                "SELECT repo_rowid FROM enrichment_queue WHERE kind = 'license'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(rowid as u64, ids[1]);
    }

    #[test]
    fn claim_returns_the_oldest_pending_and_stamps_move_state() {
        let (_scratch, mut store) = store("claim");
        seed(&mut store, &[("someone", "first"), ("someone", "second")]);
        let ids = rowids(&store);
        store
            .enqueue_missing(QueueKind::Readme, now())
            .expect("enqueue");

        let item = store.claim_pending().expect("claim").expect("an item");
        assert_eq!(item.repo_rowid, ids[0], "oldest first");
        assert_eq!(item.kind, QueueKind::Readme);
        assert_eq!(item.owner, "someone");
        assert_eq!(item.name, "first");

        store
            .stamp_skipped(item.id, &item.claim_token, "no readme exists", now())
            .expect("skip");
        assert_eq!(state_of(&store, item.id), "skipped");

        let second = store.claim_pending().expect("claim").expect("an item");
        assert_eq!(second.repo_rowid, ids[1]);
        store
            .stamp_done(second.id, &second.claim_token, now())
            .expect("done");
        assert_eq!(state_of(&store, second.id), "done");
        assert!(
            store.claim_pending().expect("claim").is_none(),
            "the queue drained"
        );
    }

    #[test]
    fn attempts_return_the_row_to_pending_until_three_strikes() {
        let (_scratch, mut store) = store("attempts");
        seed(&mut store, &[("someone", "flaky")]);
        let id = rowids(&store).remove(0);
        store
            .enqueue_missing(QueueKind::Tree, now())
            .expect("enqueue");

        for (index, error) in ["502", "timeout", "401"].iter().enumerate() {
            assert_eq!(
                state_of(&store, id),
                "pending",
                "the row waits until its third strike"
            );
            let item = store.claim_pending().expect("claim").expect("an item");
            store
                .stamp_attempt_failed(item.id, &item.claim_token, error, now())
                .expect("attempt");
            if index < 2 {
                store
                    .conn
                    .execute(
                        "UPDATE enrichment_queue SET not_before = NULL WHERE id = ?1",
                        params![item.id as i64],
                    )
                    .expect("the backoff is over");
            }
        }
        assert_eq!(state_of(&store, id), "failed", "three strikes fail");

        let (attempts, last_error): (i64, String) = store
            .conn
            .query_row(
                "SELECT attempts, last_error FROM enrichment_queue WHERE repo_rowid = ?1",
                params![id as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(attempts, 3);
        assert_eq!(last_error, "401");
    }

    #[test]
    fn queue_depths_report_the_whole_queue() {
        let (_scratch, mut store) = store("depths");
        seed(
            &mut store,
            &[("someone", "a"), ("someone", "b"), ("someone", "c")],
        );
        store
            .enqueue_missing(QueueKind::Readme, now())
            .expect("enqueue");
        store
            .enqueue_missing(QueueKind::Tree, now())
            .expect("enqueue");
        let done = store.claim_pending().expect("claim").expect("an item");
        store
            .stamp_done(done.id, &done.claim_token, now())
            .expect("done");
        let skipped = store.claim_pending().expect("claim").expect("an item");
        store
            .stamp_skipped(skipped.id, &skipped.claim_token, "gone", now())
            .expect("skip");

        let depths = store.queue_depths().expect("depths");
        assert_eq!(depths.done, 1);
        assert_eq!(depths.skipped, 1);
        assert_eq!(depths.pending, 4, "readme: one left; tree: three");
        assert_eq!(depths.failed, 0);
        assert_eq!(store.count_pending(QueueKind::Readme).expect("pending"), 1);
        assert_eq!(store.count_pending(QueueKind::Tree).expect("pending"), 3);
        assert_eq!(store.count_pending(QueueKind::License).expect("pending"), 0);
    }

    #[test]
    fn an_unknown_kind_in_the_queue_is_a_store_error() {
        let (_scratch, mut store) = store("bad-kind");
        seed(&mut store, &[("someone", "odd")]);
        store
            .conn
            .execute(
                "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
                 VALUES (1, 'postcards', '2026-10-27T00:00:00Z')",
                [],
            )
            .expect("plant the bad row");
        let error = store.claim_pending().expect_err("rejected");
        assert!(error.to_string().contains("postcards"));
    }

    /// A claim is a lease: the row is `claimed` with a future
    /// `lease_until`, other rows stay claimable beside it, and a claim
    /// whose lease lapsed — the kill mid-job — is picked up again by a
    /// reopened store.
    #[test]
    fn a_claim_holds_a_lease_and_a_killed_run_resumes_at_the_same_job() {
        let (scratch, mut store) = store("lease-resume");
        seed(&mut store, &[("someone", "first"), ("someone", "second")]);
        store
            .enqueue_missing(QueueKind::Readme, now())
            .expect("enqueue");

        let item = store.claim_pending().expect("claim").expect("an item");
        let (state, lease): (String, String) = store
            .conn
            .query_row(
                "SELECT state, lease_until FROM enrichment_queue WHERE id = ?1",
                params![item.id as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(state, "claimed");
        assert!(lease > catalog::stamp(now()), "the lease is in the future");

        let other = store.claim_pending().expect("claim").expect("an item");
        assert_ne!(other.id, item.id, "a live lease does not block the rest");

        // The kill: the lease lapses, the store reopens, and the same
        // job is claimed again.
        store
            .conn
            .execute(
                "UPDATE enrichment_queue SET lease_until = '2026-10-26T00:00:00Z'
                 WHERE id = ?1",
                params![item.id as i64],
            )
            .expect("lapse the lease");
        drop(store);
        let reopened = Store::open(&scratch.path()).expect("reopen");
        let resumed = reopened.claim_pending().expect("claim").expect("resumed");
        assert_eq!(resumed.id, item.id, "the abandoned job is picked up again");
    }

    /// The heartbeat renews a claimed row's lease while it works — the
    /// slow job keeps its claim — and never resurrects a finished row.
    #[test]
    fn the_heartbeat_extends_a_live_lease_only() {
        let (_scratch, mut store) = store("heartbeat");
        seed(&mut store, &[("someone", "slow")]);
        store
            .enqueue_missing(QueueKind::Tree, now())
            .expect("enqueue");
        let item = store.claim_pending().expect("claim").expect("an item");
        let before: String = store
            .conn
            .query_row(
                "SELECT lease_until FROM enrichment_queue WHERE id = ?1",
                params![item.id as i64],
                |row| row.get(0),
            )
            .expect("lease");
        let later = Utc::now() + chrono::Duration::minutes(5);
        assert!(
            store
                .heartbeat(item.id, &item.claim_token, later)
                .expect("heartbeat")
        );
        let after: String = store
            .conn
            .query_row(
                "SELECT lease_until FROM enrichment_queue WHERE id = ?1",
                params![item.id as i64],
                |row| row.get(0),
            )
            .expect("lease");
        assert!(
            after > before,
            "the lease moved forward: {before} → {after}"
        );
        store
            .stamp_done(item.id, &item.claim_token, now())
            .expect("done");
        assert!(
            !store
                .heartbeat(item.id, &item.claim_token, later)
                .expect("heartbeat is quiet"),
            "a done row is not the claimant's anymore"
        );
        assert_eq!(state_of(&store, item.id), "done", "a done row stays done");
    }

    /// A claim owns its row: once the lease lapses and another runner
    /// re-claims, the dispossessed runner's heartbeat and stamps change
    /// nothing — two runners cannot work one row.
    #[test]
    fn a_stale_token_heartbeats_and_stamps_nothing() {
        let (_scratch, mut store) = store("claim-token");
        seed(&mut store, &[("someone", "slow")]);
        store
            .enqueue_missing(QueueKind::Tree, now())
            .expect("enqueue");

        let first = store.claim_pending().expect("claim").expect("an item");
        // The lease lapses; a second runner takes the row.
        store
            .conn
            .execute(
                "UPDATE enrichment_queue SET lease_until = '2026-10-26T00:00:00Z' WHERE id = ?1",
                params![first.id as i64],
            )
            .expect("lapse the lease");
        let second = store.claim_pending().expect("claim").expect("re-claimed");
        assert_eq!(second.id, first.id);
        assert_ne!(second.claim_token, first.claim_token);

        assert!(
            !store
                .heartbeat(first.id, &first.claim_token, Utc::now())
                .expect("heartbeat"),
            "the stale heartbeat renews nothing"
        );
        store
            .stamp_done(first.id, &first.claim_token, now())
            .expect("stale stamp is quiet");
        assert_eq!(
            state_of(&store, first.id),
            "claimed",
            "the stale stamp changed nothing; the live claim stands"
        );
        assert!(
            store
                .heartbeat(first.id, &second.claim_token, Utc::now())
                .expect("heartbeat"),
            "the live claim still heartbeats"
        );
    }

    /// A rate-limited row is parked until its stamp passes: hidden from
    /// claims, reported by `backoff_until`, claimable again after.
    #[test]
    fn a_rate_limited_row_waits_until_its_stamp_passes() {
        let (_scratch, mut store) = store("rate-limited");
        seed(&mut store, &[("someone", "a"), ("someone", "b")]);
        store
            .enqueue_missing(QueueKind::Readme, now())
            .expect("enqueue");

        let item = store.claim_pending().expect("claim").expect("an item");
        let reset = Utc::now() + chrono::Duration::hours(1);
        store
            .stamp_rate_limited(item.id, &item.claim_token, reset, Utc::now())
            .expect("park");
        assert_eq!(state_of(&store, item.id), "pending", "no attempt burned");

        let second = store.claim_pending().expect("claim").expect("an item");
        assert_ne!(second.id, item.id, "the parked row is skipped");
        assert!(
            store.claim_pending().expect("claim").is_none(),
            "nothing else is eligible"
        );
        assert_eq!(
            store
                .backoff_until()
                .expect("backoff")
                .expect("a wait")
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            reset.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "the wait is the quota's reset"
        );

        store
            .conn
            .execute(
                "UPDATE enrichment_queue SET not_before = NULL WHERE id = ?1",
                params![item.id as i64],
            )
            .expect("the reset has passed");
        let again = store.claim_pending().expect("claim").expect("again");
        assert_eq!(again.id, item.id, "the row is claimable after the reset");
    }

    /// A failed attempt backs off before the row is claimable again;
    /// the third strike fails it and clears the wait.
    #[test]
    fn a_failed_attempt_backs_off_until_its_third_strike_fails_it() {
        let (_scratch, mut store) = store("attempt-backoff");
        seed(&mut store, &[("someone", "flaky")]);
        store
            .enqueue_missing(QueueKind::Tree, now())
            .expect("enqueue");
        let item = store.claim_pending().expect("claim").expect("an item");

        store
            .stamp_attempt_failed(item.id, &item.claim_token, "502", Utc::now())
            .expect("attempt 1");
        assert_eq!(state_of(&store, item.id), "pending");
        assert!(
            store.claim_pending().expect("claim").is_none(),
            "the backed-off row is not claimable yet"
        );
        let wait = store.backoff_until().expect("backoff").expect("a wait");
        assert!(wait > Utc::now(), "the backoff is in the future: {wait}");

        for error in ["timeout", "401"] {
            store
                .conn
                .execute(
                    "UPDATE enrichment_queue SET not_before = NULL WHERE id = ?1",
                    params![item.id as i64],
                )
                .expect("the backoff is over");
            let again = store.claim_pending().expect("claim").expect("an item");
            store
                .stamp_attempt_failed(again.id, &again.claim_token, error, Utc::now())
                .expect("attempt");
        }
        assert_eq!(state_of(&store, item.id), "failed", "three strikes");
        assert!(
            store.backoff_until().expect("backoff").is_none(),
            "a failed row holds no wait"
        );
    }

    #[test]
    fn the_readmes_full_text_row_follows_the_body_update() {
        let (_scratch, mut store) = store("readme-fts");
        seed(&mut store, &[("someone", "indexed")]);
        let id = rowids(&store).remove(0);
        store
            .store_readme(id, "the alpha document\n", now())
            .expect("stored");
        let count = |store: &Store, word: &str| -> i64 {
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM fts_readme WHERE fts_readme MATCH ?1",
                    params![word],
                    |row| row.get(0),
                )
                .expect("indexed")
        };
        assert_eq!(count(&store, "alpha"), 1);
        store
            .store_readme(id, "the beta document\n", now())
            .expect("replaced");
        assert_eq!(
            count(&store, "alpha"),
            0,
            "the replaced body's words are gone"
        );
        assert_eq!(
            count(&store, "beta"),
            1,
            "the new body's words are searchable"
        );
    }

    #[test]
    fn the_unicode61_tokenizer_carries_cjk_runs_whole() {
        let (_scratch, mut store) = store("readme-cjk");
        seed(&mut store, &[("someone", "tokyo")]);
        let id = rowids(&store).remove(0);
        store
            .store_readme(id, "このリポジトリは端末を録画します\n", now())
            .expect("stored");
        // A contiguous run matches as the tokenizer stored it; a
        // fragment inside the run does not (no per-character
        // segmentation — the tokenizer is named in the schema).
        let count = |word: &str| -> i64 {
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM fts_readme WHERE fts_readme MATCH ?1",
                    params![word],
                    |row| row.get(0),
                )
                .expect("indexed")
        };
        assert_eq!(count("このリポジトリは端末を録画します"), 1);
        assert_eq!(count("端末"), 0, "a fragment is not a token");
    }
}
