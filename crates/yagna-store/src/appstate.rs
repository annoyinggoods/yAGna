//! The `app_state` table: the small key-value store the app keeps for
//! itself — the state the user chose to persist (the expanded rows'
//! set, the sidebar's open drawer) and the counters the stats line
//! reports (the cache eviction totals), as plain texts under stable
//! keys.
//!
//! Items: `EXPANDED_ROWS_KEY` — the key the expansion set lives under;
//! `SIDEBAR_DRAWER_KEY` — the key the sidebar's open drawer lives
//! under; `EVICTED_FILES_KEY`/`EVICTED_BYTES_KEY` — the eviction
//! counters.
//! Depends on: rusqlite (the connection).
//! Called by: crate::lib (the Store facade), the app crate's list and
//! stats verb.

use crate::{Store, StoreError};

/// The key the persisted expansion set lives under: a JSON array of
/// repository rowids.
pub const EXPANDED_ROWS_KEY: &str = "expanded_rows";

/// The key the sidebar's open drawer lives under: the drawer's name,
/// or no key at all when the sidebar holds every drawer closed.
pub const SIDEBAR_DRAWER_KEY: &str = "sidebar_drawer";

/// The key the total evicted asset/digest count accumulates under.
pub const EVICTED_FILES_KEY: &str = "evicted_files";

/// The key the total evicted byte count accumulates under.
pub const EVICTED_BYTES_KEY: &str = "evicted_bytes";

/// The key the last fit re-score's instant lives under — the score
/// surface's "last re-scored" line.
pub const FIT_RESCORED_AT_KEY: &str = "fit_rescored_at";

/// The key the last fit re-score's moved-row count lives under — how
/// many rows the last re-score changed.
pub const FIT_RESCORED_MOVED_KEY: &str = "fit_rescored_moved";

/// The key the last fit re-score's covered-row count lives under.
pub const FIT_RESCORED_ROWS_KEY: &str = "fit_rescored_rows";

impl Store {
    /// Writes one state key, replacing whatever it held.
    pub fn set_app_state(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// Reads one state key, or `None` when the app never wrote it.
    pub fn app_state(&self, key: &str) -> Result<Option<String>, StoreError> {
        let value =
            self.conn
                .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                    row.get::<_, String>(0)
                });
        match value {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Reads one state key as a counter — `0` when it was never written
    /// or does not parse: a counter that cannot be read reports zero,
    /// it never fails the caller.
    pub fn app_state_counter(&self, key: &str) -> Result<u64, StoreError> {
        Ok(self
            .app_state(key)?
            .and_then(|text| text.parse().ok())
            .unwrap_or(0))
    }

    /// Adds `by` to one state counter.
    pub fn bump_app_state_counter(&self, key: &str, by: u64) -> Result<(), StoreError> {
        let current = self.app_state_counter(key)?;
        self.set_app_state(key, &(current + by).to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use crate::testing::Scratch;

    /// A key written and read back is the same text; a key never
    /// written reads as absent.
    #[test]
    fn state_round_trips_and_absent_keys_read_as_none() {
        let scratch = Scratch::new("app-state");
        let store = Store::open(&scratch.path()).expect("open");
        assert_eq!(store.app_state("expanded_rows").expect("read"), None);
        store
            .set_app_state("expanded_rows", "[3,1,2]")
            .expect("write");
        assert_eq!(
            store.app_state("expanded_rows").expect("read"),
            Some("[3,1,2]".to_string())
        );
        store
            .set_app_state("expanded_rows", "[7]")
            .expect("rewrite");
        assert_eq!(
            store.app_state("expanded_rows").expect("read"),
            Some("[7]".to_string())
        );
    }
}
