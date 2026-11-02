//! The sync_runs ledger: one row per completed sweep, whatever its
//! outcome — the record a later run and the stats line read back.
//!
//! Items: `SyncRunRecord` — one run's facts; `KIND_FULL` — the v1 sweep's
//! kind; `record_sync_run`, `count_runs`, `latest_run` — the ledger
//! operations.
//! Depends on: chrono (timestamps).
//! Called by: crate::lib (the Store facade) and the sync verb.

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::{Store, StoreError, catalog};

/// The kind value of the v1 sweep: everything, every time. A delta sweep
/// would be a new kind, decided where it is built.
pub const KIND_FULL: &str = "full";

/// One completed sync run: when it ran, what it was, what it saw, and —
/// when it failed — why. A failed run has no `finished_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRunRecord {
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub kind: String,
    pub pages: Option<u64>,
    pub repos_seen: u64,
    pub repos_upserted: u64,
    pub repos_new: u64,
    pub repos_missing: u64,
    pub error: Option<String>,
}

impl Store {
    /// Appends one run to the ledger.
    pub fn record_sync_run(&mut self, record: &SyncRunRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sync_runs (
               started_at, finished_at, kind, pages,
               repos_seen, repos_upserted, repos_new, repos_missing, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                catalog::stamp(record.started_at),
                record.finished_at.map(catalog::stamp),
                record.kind,
                record.pages.map(catalog::saturating_i64),
                catalog::saturating_i64(record.repos_seen),
                catalog::saturating_i64(record.repos_upserted),
                catalog::saturating_i64(record.repos_new),
                catalog::saturating_i64(record.repos_missing),
                record.error,
            ],
        )?;
        Ok(())
    }

    /// How many runs the ledger holds.
    pub fn count_runs(&self) -> Result<u64, StoreError> {
        let count = self
            .conn
            .query_row("SELECT COUNT(*) FROM sync_runs", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count as u64)
    }

    /// The most recent run, if any — the ledger's newest row.
    pub fn latest_run(&self) -> Result<Option<SyncRunRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT started_at, finished_at, kind, pages,
                    repos_seen, repos_upserted, repos_new, repos_missing, error
             FROM sync_runs ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = statement.query([])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(parse_run(row)?)),
        }
    }
}

/// Reads one ledger row back. Timestamps round-trip through the store's
/// one format; an unparseable pair is a store error, not a silent row.
fn parse_run(row: &rusqlite::Row<'_>) -> Result<SyncRunRecord, StoreError> {
    let started_at = row.get::<_, String>(0)?;
    let finished_at: Option<String> = row.get(1)?;
    Ok(SyncRunRecord {
        started_at: catalog::parse_stamp(&started_at)?,
        finished_at: finished_at
            .as_deref()
            .map(catalog::parse_stamp)
            .transpose()?,
        kind: row.get(2)?,
        pages: row
            .get::<_, Option<i64>>(3)?
            .map(|pages| pages.max(0) as u64),
        repos_seen: row.get::<_, i64>(4)?.max(0) as u64,
        repos_upserted: row.get::<_, i64>(5)?.max(0) as u64,
        repos_new: row
            .get::<_, Option<i64>>(6)?
            .map(|count| count.max(0) as u64)
            .unwrap_or(0),
        repos_missing: row
            .get::<_, Option<i64>>(7)?
            .map(|count| count.max(0) as u64)
            .unwrap_or(0),
        error: row.get(8)?,
    })
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

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    #[test]
    fn runs_roundtrip_and_read_back_newest_first() {
        let (_scratch, mut store) = store("runs");
        assert_eq!(store.latest_run().expect("latest"), None);
        assert_eq!(store.count_runs().expect("count"), 0);

        let first = SyncRunRecord {
            started_at: date(2026, 10, 20),
            finished_at: Some(date(2026, 10, 20)),
            kind: KIND_FULL.to_string(),
            pages: Some(53),
            repos_seen: 1_987,
            repos_upserted: 1_987,
            repos_new: 12,
            repos_missing: 3,
            error: None,
        };
        store.record_sync_run(&first).expect("record");
        let failed = SyncRunRecord {
            started_at: date(2026, 10, 21),
            finished_at: None,
            kind: KIND_FULL.to_string(),
            pages: Some(28),
            repos_seen: 1_400,
            repos_upserted: 1_400,
            repos_new: 0,
            repos_missing: 0,
            error: Some("GitHub answered 401: Bad credentials".to_string()),
        };
        store.record_sync_run(&failed).expect("record");

        assert_eq!(store.count_runs().expect("count"), 2);
        let latest = store.latest_run().expect("latest").expect("a run exists");
        assert_eq!(latest, failed, "the newest run is the failed one");
        assert_eq!(
            latest.error.as_deref(),
            Some("GitHub answered 401: Bad credentials")
        );
        assert_eq!(latest.pages, Some(28));
        assert_eq!(latest.finished_at, None, "a failed run has no end");
    }
}
