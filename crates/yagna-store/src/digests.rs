//! The digest artifact: its storage, its read side, its per-repository
//! enqueue, and the stale stamping the sweep applies.
//!
//! A digest is cache, not catalog: one row per repository that has ever
//! had one taken, with the spec it was taken under and the branch head
//! it was cut from. The sweep's upsert stamps `stale_at` when the
//! repository's last commit moved (see `stamp_stale`); a stale digest is
//! regenerated on the next open, exactly once. The body's full-text row
//! is kept in step inside the same transaction that writes the body —
//! the external-content delete/reinsert dance, invisible to callers.
//!
//! Items: `DigestBody` — the stored digest's read side; `store_digest`
//! — the format-and-store verb; `digest_body` — the stored record;
//! `enqueue_digest` — the tab's demand-only queue fill;
//! `claim_pending_for` (the queue's own) serves it;
//! `stamp_stale` — the sweep's invalidation.
//! Depends on: rusqlite, chrono, serde_json (the spec column),
//! yagna_core::digest (the formatting and the token estimate),
//! yagna_core::model (the spec), crate::catalog (the stamp format).
//! Called by: crate::lib (the Store facade) and the app's digest
//! pipeline.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use yagna_core::digest::{self, DigestInput};
use yagna_core::model::DigestSpec;

use crate::{Store, StoreError, catalog, saturating_rowid};

/// The stored digest's read side: the body the tab renders, the token
/// estimate its summary promised, the cap that stopped its walk, when
/// and from what it was taken, and when the sweep marked it stale —
/// none while it is fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestBody {
    pub body: String,
    pub token_estimate: Option<u64>,
    pub truncated: bool,
    pub taken_at: DateTime<Utc>,
    pub taken_commit: Option<String>,
    pub stale_at: Option<DateTime<Utc>>,
}

impl Store {
    /// Formats and stores one repository's digest: the input filtered
    /// by the spec, formatted through the core's formatter, the row
    /// written (a fresh taken_at, the stale mark cleared), and the
    /// full-text row re-indexed — all in one transaction. The stored
    /// spec is what the digest was actually taken under.
    pub fn store_digest(
        &mut self,
        repo_rowid: u64,
        spec: &DigestSpec,
        input: &DigestInput,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let spec_json = serde_json::to_string(spec)?;
        let body = digest::format_digest(input);
        let token_estimate = i64::try_from(digest::token_estimate(input)).unwrap_or(i64::MAX);
        let truncated = i64::from(input.truncated_by.is_some());
        let taken_commit = input.taken_commit.as_deref();
        let transaction = self.conn.transaction()?;
        delete_fts_row(&transaction, saturating_rowid(repo_rowid))?;
        transaction.execute(
            "INSERT OR REPLACE INTO digests
               (repo_rowid, spec_json, body, token_estimate, truncated, taken_at, taken_commit, stale_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                saturating_rowid(repo_rowid),
                spec_json,
                body,
                token_estimate,
                truncated,
                catalog::stamp(now),
                taken_commit,
            ],
        )?;
        transaction.execute(
            "INSERT INTO fts_digest(rowid, body) VALUES (?1, ?2)",
            params![saturating_rowid(repo_rowid), body],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The stored digest, when the repository has one — stale or fresh.
    pub fn digest_body(&self, repo_rowid: u64) -> Result<Option<DigestBody>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT body, token_estimate, truncated, taken_at, taken_commit, stale_at
               FROM digests WHERE repo_rowid = ?1",
        )?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let token_estimate: Option<i64> = row.get(1)?;
        let stale_at: Option<String> = row.get(5)?;
        Ok(Some(DigestBody {
            body: row.get(0)?,
            token_estimate: token_estimate.map(|tokens| tokens.max(0) as u64),
            truncated: row.get::<_, i64>(2)? != 0,
            taken_at: catalog::parse_stamp(&row.get::<_, String>(3)?)?,
            taken_commit: row.get(4)?,
            stale_at: match stale_at {
                Some(text) => Some(catalog::parse_stamp(&text)?),
                None => None,
            },
        }))
    }

    /// Enqueues one repository's digest — the tab's Generate action,
    /// demand-only. A pending-or-claimed row blocks a duplicate (a
    /// claimed one is being generated right now); a fresh digest does
    /// not regenerate; a stale digest does. A repository without a
    /// default branch has nothing to digest.
    pub fn enqueue_digest(
        &mut self,
        repo_rowid: u64,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO enrichment_queue (repo_rowid, kind, updated_at)
             SELECT ?1, 'digest', ?2
              WHERE EXISTS (
                SELECT 1 FROM repos WHERE id = ?1 AND default_branch IS NOT NULL
              )
              AND NOT EXISTS (
                SELECT 1 FROM enrichment_queue
                 WHERE enrichment_queue.repo_rowid = ?1
                   AND enrichment_queue.kind = 'digest'
                   AND enrichment_queue.state IN ('pending', 'claimed')
              )
              AND (
                NOT EXISTS (SELECT 1 FROM digests WHERE digests.repo_rowid = ?1)
                OR (SELECT stale_at FROM digests WHERE digests.repo_rowid = ?1) IS NOT NULL
              )",
            params![saturating_rowid(repo_rowid), catalog::stamp(now)],
        )?;
        Ok(inserted > 0)
    }
}

/// Deletes one digest's full-text twin — the external-content index's
/// delete half of the dance, shared by the re-store and the eviction.
fn delete_fts_row(conn: &Connection, repo_rowid: i64) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO fts_digest(fts_digest, rowid, body)
         SELECT 'delete', repo_rowid, body FROM digests WHERE repo_rowid = ?1",
        params![repo_rowid],
    )
}

/// The bytes the digest cache holds: the spec, the body and the commit
/// stamp of every stored row, summed — the digest side of the cache
/// cap.
pub(crate) fn stored_bytes(conn: &Connection) -> u64 {
    conn.query_row(
        "SELECT COALESCE(SUM(LENGTH(spec_json) + LENGTH(body)
                        + COALESCE(LENGTH(taken_commit), 0)), 0)
           FROM digests",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|bytes| bytes.max(0) as u64)
    .unwrap_or(0)
}

/// Evicts the oldest digest — by `taken_at` — deleting its row and its
/// full-text twin, and returns the bytes it held. `None` when the
/// digest cache is empty: the eviction walk stops.
pub(crate) fn evict_oldest(conn: &Connection) -> Result<Option<u64>, StoreError> {
    let target: Option<i64> = conn
        .query_row(
            "SELECT repo_rowid FROM digests ORDER BY taken_at ASC, repo_rowid ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(repo_rowid) = target else {
        return Ok(None);
    };
    let freed: i64 = conn.query_row(
        "SELECT COALESCE(LENGTH(spec_json), 0) + COALESCE(LENGTH(body), 0)
                + COALESCE(LENGTH(taken_commit), 0)
           FROM digests WHERE repo_rowid = ?1",
        params![repo_rowid],
        |row| row.get(0),
    )?;
    let transaction = conn.unchecked_transaction()?;
    delete_fts_row(&transaction, repo_rowid)?;
    transaction.execute(
        "DELETE FROM digests WHERE repo_rowid = ?1",
        params![repo_rowid],
    )?;
    transaction.commit()?;
    Ok(Some(freed.max(0) as u64))
}

/// Marks one repository's digest stale — the sweep's move, run on the
/// upsert's own transaction by `catalog::upsert_snapshots` when the
/// incoming last-commit stamp differs from the stored one. A repository
/// without a digest is untouched; a digest already stale keeps its
/// first stale stamp.
pub(crate) fn stamp_stale(conn: &Connection, repo_rowid: i64, stamp: &str) -> bool {
    conn.execute(
        "UPDATE digests SET stale_at = ?2
          WHERE repo_rowid = ?1 AND stale_at IS NULL",
        params![repo_rowid, stamp],
    )
    .map(|written| written > 0)
    .unwrap_or(false)
}

/// The sweep's pre-read: the stored last-commit stamp of one repository
/// identity, when it is in the catalog at all. The upsert compares this
/// against the incoming stamp and stales the digest on a move — after
/// the upsert the stored value is already the new one, so the read
/// happens first.
pub(crate) fn stored_last_commit(
    conn: &Connection,
    forge: &str,
    owner: &str,
    name: &str,
) -> Option<(i64, String)> {
    conn.query_row(
        "SELECT id, last_commit_at FROM repos WHERE forge = ?1 AND owner = ?2 AND name = ?3",
        params![forge, owner, name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueueKind;
    use crate::testing::{Scratch, sample_snapshot};
    use yagna_core::model::{Forge, RepoRef};

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

    fn later() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 30, 9, 0, 0)
            .single()
            .expect("valid test instant")
    }

    fn repo() -> RepoRef {
        RepoRef {
            forge: Forge::GitHub,
            owner: "someone".to_string(),
            name: "something".to_string(),
        }
    }

    fn file(path: &str, text: &str) -> yagna_core::digest::DigestFile {
        yagna_core::digest::DigestFile {
            path: path.to_string(),
            bytes: text.len() as u64,
            text: Some(text.to_string()),
        }
    }

    fn input() -> DigestInput {
        DigestInput {
            repo: repo(),
            files: vec![
                file("README.md", "# Demo\n"),
                file("src/main.rs", "fn main() {}\n"),
            ],
            truncated_by: None,
            taken_commit: Some("abc123".to_string()),
        }
    }

    fn seeded_id(store: &mut Store) -> u64 {
        store
            .upsert_snapshots(&[sample_snapshot("someone", "something")], now())
            .expect("seed");
        store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get::<_, i64>(0))
            .expect("rowid") as u64
    }

    #[test]
    fn the_digest_roundtrips_with_its_provenance() {
        let (_scratch, mut store) = store("digest-roundtrip");
        let id = seeded_id(&mut store);
        let spec = digest::standard_spec(&repo());
        store
            .store_digest(id, &spec, &input(), now())
            .expect("stored");
        let body = store.digest_body(id).expect("read").expect("a digest");
        assert!(body.body.starts_with("Repository: someone/something\n"));
        assert!(body.token_estimate.is_some());
        assert!(!body.truncated);
        assert_eq!(body.taken_at, now());
        assert_eq!(body.taken_commit.as_deref(), Some("abc123"));
        assert!(body.stale_at.is_none(), "a fresh digest is fresh");
        assert!(
            store.digest_body(999).expect("read").is_none(),
            "a repository without a digest has none"
        );
    }

    #[test]
    fn a_second_take_clears_the_stale_mark_and_replaces_the_body() {
        let (_scratch, mut store) = store("digest-retake");
        let id = seeded_id(&mut store);
        let spec = digest::standard_spec(&repo());
        store
            .store_digest(id, &spec, &input(), now())
            .expect("stored");
        assert!(
            stamp_stale(&store.conn, id as i64, &crate::catalog::stamp(later())),
            "the sweep stales it"
        );
        assert!(
            store
                .digest_body(id)
                .expect("read")
                .expect("a digest")
                .stale_at
                .is_some()
        );
        let retaken = DigestInput {
            files: vec![file("NEW.md", "newer\n")],
            ..input()
        };
        store
            .store_digest(id, &spec, &retaken, later())
            .expect("restored");
        let body = store.digest_body(id).expect("read").expect("a digest");
        assert!(
            body.body.contains("NEW.md"),
            "the new body replaced the old"
        );
        assert_eq!(body.taken_at, later());
        assert!(body.stale_at.is_none(), "the retake is fresh again");
    }

    #[test]
    fn the_digests_full_text_row_follows_the_body() {
        let (_scratch, mut store) = store("digest-fts");
        let id = seeded_id(&mut store);
        let spec = digest::standard_spec(&repo());
        store
            .store_digest(id, &spec, &input(), now())
            .expect("stored");
        let hits: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_digest WHERE fts_digest MATCH 'demo'",
                [],
                |row| row.get(0),
            )
            .expect("indexed");
        assert_eq!(hits, 1, "the body's words are searchable");
        let retaken = DigestInput {
            files: vec![file("OTHER.md", "nothing to see")],
            ..input()
        };
        store
            .store_digest(id, &spec, &retaken, later())
            .expect("restored");
        let stale_hits: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_digest WHERE fts_digest MATCH 'demo'",
                [],
                |row| row.get(0),
            )
            .expect("indexed");
        assert_eq!(stale_hits, 0, "the replaced body's words are gone");
        let fresh_hits: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_digest WHERE fts_digest MATCH 'nothing'",
                [],
                |row| row.get(0),
            )
            .expect("indexed");
        assert_eq!(fresh_hits, 1, "the new body's words are searchable");
    }

    #[test]
    fn the_digest_enqueue_is_demand_only_and_reenqueues_stale() {
        let (_scratch, mut store) = store("digest-enqueue");
        let id = seeded_id(&mut store);
        assert!(
            store.enqueue_digest(id, now()).expect("enqueue"),
            "a repository with no digest enqueues"
        );
        assert!(
            !store.enqueue_digest(id, now()).expect("enqueue"),
            "a live queue row is not duplicated"
        );
        // A fresh digest (a done row, no stale mark) does not re-enqueue.
        let item = store
            .claim_pending_for(id, QueueKind::Digest)
            .expect("claim")
            .expect("the digest row");
        let spec = digest::standard_spec(&repo());
        store
            .store_digest(id, &spec, &input(), now())
            .expect("stored");
        store
            .stamp_done(item.id, &item.claim_token, now())
            .expect("stamped");
        assert!(
            !store.enqueue_digest(id, now()).expect("enqueue"),
            "a fresh digest does not regenerate"
        );
        // A stale digest does.
        assert!(
            stamp_stale(&store.conn, id as i64, &crate::catalog::stamp(later())),
            "staled"
        );
        assert!(
            store.enqueue_digest(id, later()).expect("enqueue"),
            "a stale digest regenerates"
        );
    }

    #[test]
    fn a_killed_digest_run_resumes_at_the_same_job() {
        let (_scratch, mut store) = store("digest-resume");
        let id = seeded_id(&mut store);
        assert!(
            store.enqueue_digest(id, now()).expect("enqueue"),
            "the digest row is queued"
        );
        let item = store
            .claim_pending_for(id, QueueKind::Digest)
            .expect("claim")
            .expect("the digest row");
        assert_eq!(item.kind, QueueKind::Digest);

        // The kill: the lease lapses, and the next run claims the same
        // job — the digest is not lost, only paused.
        store
            .conn
            .execute(
                "UPDATE enrichment_queue SET lease_until = '2026-01-01T00:00:00Z'
                 WHERE id = ?1",
                params![item.id as i64],
            )
            .expect("lapse the lease");
        let resumed = store
            .claim_pending_for(id, QueueKind::Digest)
            .expect("claim")
            .expect("resumed");
        assert_eq!(resumed.id, item.id, "the same job is picked up again");
    }

    #[test]
    fn the_digest_enqueue_refuses_a_repository_without_a_branch() {
        let (_scratch, mut store) = store("digest-no-branch");
        store
            .upsert_snapshots(&[sample_snapshot("someone", "empty")], now())
            .expect("seed");
        let id: i64 = store
            .conn
            .query_row("SELECT id FROM repos", [], |row| row.get(0))
            .expect("rowid");
        store
            .conn
            .execute(
                "UPDATE repos SET default_branch = NULL WHERE id = ?1",
                params![id],
            )
            .expect("branched away");
        assert!(
            !store.enqueue_digest(id as u64, now()).expect("enqueue"),
            "an empty repository has nothing to digest"
        );
    }

    #[test]
    fn the_bulk_fill_refuses_the_digest_kind() {
        let (_scratch, mut store) = store("digest-bulk-guard");
        seeded_id(&mut store);
        let error = store
            .enqueue_missing(QueueKind::Digest, now())
            .expect_err("refused");
        assert!(
            error.to_string().contains("never pre-generated"),
            "the guard says why: {error}"
        );
    }

    #[test]
    fn a_sweep_that_moves_last_commit_stamps_the_digest_stale() {
        let (_scratch, mut store) = store("digest-sweep-stale");
        let id = seeded_id(&mut store);
        let spec = digest::standard_spec(&repo());
        store
            .store_digest(id, &spec, &input(), now())
            .expect("stored");

        // The same commit comes around again in the next sweep: the
        // digest stays fresh.
        let mut snapshot = sample_snapshot("someone", "something");
        store
            .upsert_snapshots(std::slice::from_ref(&snapshot), later())
            .expect("resweeped");
        let body = store.digest_body(id).expect("read").expect("a digest");
        assert!(body.stale_at.is_none(), "an unmoved repository is fresh");

        // The repository moves: the digest is stale, once, exactly.
        snapshot.last_commit_at = later();
        store
            .upsert_snapshots(std::slice::from_ref(&snapshot), later())
            .expect("resweeped");
        let body = store.digest_body(id).expect("read").expect("a digest");
        assert!(
            body.stale_at.is_some(),
            "a moved repository stales its digest"
        );

        // And the stale digest re-enqueues for exactly one regeneration.
        assert!(
            store.enqueue_digest(id, later()).expect("enqueue"),
            "the stale digest regenerates"
        );
        assert!(
            !store.enqueue_digest(id, later()).expect("enqueue"),
            "and only once"
        );
    }
}
