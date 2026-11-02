//! The read-only shadows: other users' starred repositories and the
//! DISCOVER results, scoped by login — never mixed into the catalog.
//!
//! The distinction is structural: shadows live in `shadow_repos`, the
//! catalog's queries never read that table, so no shadow row is ever
//! answerable by a catalog surface (the page, the fuzzy index, the FTS
//! search, the stats). `followed_users` is the PEOPLE tab's list with
//! its last-fetch stamp. One refresh replaces one login's rows, capped
//! at `MAX_SHADOWS_PER_LOGIN` by stars.
//!
//! Items: `Store::{follow_user, unfollow_user, followed_users,
//! followed_stamp, store_shadows, shadows_for, shadow_count}` — the
//! PEOPLE and DISCOVER paths.
//! Depends on: rusqlite (the tables), chrono (the stamps),
//! yagna_core::model (the snapshots and shadows).
//! Called by: the app crate (the PEOPLE and DISCOVER tabs).

use chrono::{DateTime, Utc};
use rusqlite::params;
use yagna_core::model::{Forge, MAX_SHADOWS_PER_LOGIN, RepoSnapshot, ShadowRepo};

use crate::catalog::{parse_stamp, saturating_i64, stamp};
use crate::{Store, StoreError};

/// The DISCOVER results' login: the search is nobody's starred list,
/// so it scopes under a login no GitHub user can own.
pub const DISCOVER_LOGIN: &str = "@discover";

impl Store {
    /// Follows one login: the PEOPLE tab's list gains it. Following
    /// twice is a non-event; the stamp is left alone.
    pub fn follow_user(&mut self, login: &str) -> Result<(), StoreError> {
        let trimmed = login.trim().to_string();
        if !yagna_core::model::is_followable_login(&trimmed) {
            return Err(StoreError::Parse(format!(
                "cannot follow {login:?}: not a GitHub username"
            )));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO followed_users (login) VALUES (?1)",
            params![trimmed],
        )?;
        Ok(())
    }

    /// Unfollows one login and drops its shadows: the views go with
    /// the follow.
    pub fn unfollow_user(&mut self, login: &str) -> Result<(), StoreError> {
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "DELETE FROM followed_users WHERE login = ?1",
            params![login.trim()],
        )?;
        transaction.execute(
            "DELETE FROM shadow_repos WHERE login = ?1",
            params![login.trim()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every followed login, in case-insensitive order.
    pub fn followed_users(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT login FROM followed_users ORDER BY login COLLATE NOCASE ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut logins = Vec::new();
        for row in rows {
            logins.push(row?);
        }
        Ok(logins)
    }

    /// One login's last-fetch stamp, when it has one.
    pub fn followed_stamp(&self, login: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        let stamp: Option<String> = self
            .conn
            .query_row(
                "SELECT fetched_at FROM followed_users WHERE login = ?1",
                params![login.trim()],
                |row| row.get(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StoreError::Parse(format!("{login:?} is not followed"))
                }
                other => StoreError::Sqlite(other),
            })?;
        match stamp {
            Some(text) => Ok(Some(parse_stamp(&text)?)),
            None => Ok(None),
        }
    }

    /// Stores one login's shadows: the refresh replaces the login's
    /// rows whole (the fetched-at stamp is the refresh's instant),
    /// keeps the top `MAX_SHADOWS_PER_LOGIN` by stars, and stamps the
    /// followed list. Shadows carry no health, no tags, no enrichment.
    pub fn store_shadows(
        &mut self,
        login: &str,
        snapshots: &[RepoSnapshot],
        now: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let login = login.trim().to_string();
        let transaction = self.conn.transaction()?;
        transaction.execute("DELETE FROM shadow_repos WHERE login = ?1", params![login])?;
        let mut kept: Vec<&RepoSnapshot> = snapshots.iter().collect();
        kept.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.stars));
        kept.truncate(MAX_SHADOWS_PER_LOGIN);
        let fetched = stamp(now);
        {
            let mut statement = transaction.prepare(
                "INSERT INTO shadow_repos (
                   login, forge, owner, name, description, stars,
                   primary_language, languages_json,
                   license_name, license_spdx, topics_json, default_branch,
                   created_at, last_commit_at, fetched_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for snapshot in &kept {
                statement.execute(params![
                    login,
                    snapshot.forge.as_str(),
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
                    stamp(snapshot.last_commit_at),
                    fetched,
                ])?;
            }
        }
        transaction.execute(
            "UPDATE followed_users SET fetched_at = ?1 WHERE login = ?2",
            params![fetched, login],
        )?;
        transaction.commit()?;
        Ok(kept.len() as u64)
    }

    /// One login's shadows, most stars first — the PEOPLE list's
    /// triple (name, languages, stars) plus what the row shows.
    pub fn shadows_for(&self, login: &str) -> Result<Vec<ShadowRepo>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT forge, owner, name, description, stars,
                    primary_language, languages_json,
                    license_name, license_spdx, topics_json, default_branch,
                    created_at, last_commit_at, fetched_at
             FROM shadow_repos WHERE login = ?1 ORDER BY stars DESC",
        )?;
        let rows = statement.query_map(params![login.trim()], map_shadow_row)?;
        let mut shadows = Vec::new();
        for row in rows {
            shadows.push(row.map_err(map_shadow_error)?);
        }
        let login = login.trim().to_string();
        shadows
            .into_iter()
            .map(|(snapshot, fetched_at)| {
                Ok(ShadowRepo {
                    login: login.clone(),
                    snapshot,
                    fetched_at,
                })
            })
            .collect()
    }

    /// How many shadow rows one login holds.
    pub fn shadow_count(&self, login: &str) -> Result<u64, StoreError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM shadow_repos WHERE login = ?1",
            params![login.trim()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }
}

/// Maps one shadow row into its snapshot and stamp. Every parse
/// failure is a `StoreError::Parse`: the rows were written by this
/// store, so a value that does not read back breaks the schema's
/// promise.
fn map_shadow_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(RepoSnapshot, DateTime<Utc>)> {
    map_shadow_inner(row).map_err(|error| match error {
        StoreError::Sqlite(inner) => inner,
        other => rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(other),
        ),
    })
}

/// Carries a mapping failure out of `query_map`.
fn map_shadow_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::FromSqlConversionFailure(_, _, source) => source
            .downcast::<StoreError>()
            .map(|boxed| *boxed)
            .unwrap_or_else(|source| StoreError::Parse(source.to_string())),
        other => StoreError::Sqlite(other),
    }
}

fn map_shadow_inner(row: &rusqlite::Row<'_>) -> Result<(RepoSnapshot, DateTime<Utc>), StoreError> {
    let forge_text: String = row.get(0)?;
    if forge_text.as_str() != Forge::GitHub.as_str() {
        return Err(StoreError::Parse(format!(
            "cannot read shadow forge {forge_text:?}"
        )));
    }
    let languages_json: Option<String> = row.get(6)?;
    let languages = match languages_json {
        Some(text) => serde_json::from_str(&text)?,
        None => Vec::new(),
    };
    let topics_json: Option<String> = row.get(9)?;
    let topics = match topics_json {
        Some(text) => serde_json::from_str(&text)?,
        None => Vec::new(),
    };
    let created_text: String = row.get(11)?;
    let commit_text: String = row.get(12)?;
    let fetched_text: String = row.get(13)?;
    let license_name: Option<String> = row.get(7)?;
    let license = match license_name {
        Some(name) => Some(yagna_core::model::LicenseInfo {
            name,
            spdx_id: row.get(8)?,
        }),
        None => None,
    };
    let snapshot = RepoSnapshot {
        forge: Forge::GitHub,
        repo_id: 0,
        owner: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        stars: row.get::<_, i64>(4)?.max(0) as u64,
        languages,
        license,
        topics,
        default_branch: row.get(10)?,
        created_at: parse_stamp(&created_text)?,
        last_commit_at: parse_stamp(&commit_text)?,
        starred_at: None,
        commit_count: 0,
        latest_release: None,
        is_fork: false,
        is_archived: false,
    };
    Ok((snapshot, parse_stamp(&fetched_text)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CatalogFilter;
    use crate::testing::{Scratch, sample_snapshot};
    use chrono::TimeZone;
    use yagna_core::model::SortKey;

    fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn shadow_snapshot(owner: &str, name: &str, stars: u64) -> RepoSnapshot {
        let mut snapshot = sample_snapshot(owner, name);
        snapshot.stars = stars;
        snapshot
    }

    #[test]
    fn follow_lists_and_unfollow_drops_its_shadows() {
        let scratch = Scratch::new("follow");
        let mut store = Store::open(&scratch.path()).expect("open");
        store.follow_user("Friend").expect("follow");
        store.follow_user("Friend").expect("idempotent");
        assert_eq!(store.followed_users().expect("list"), ["Friend"]);
        store
            .store_shadows("Friend", &[shadow_snapshot("a", "b", 1)], at(2026, 10, 30))
            .expect("store");
        assert_eq!(store.shadow_count("Friend").expect("count"), 1);
        store.unfollow_user("Friend").expect("unfollow");
        assert!(store.followed_users().expect("list").is_empty());
        assert_eq!(store.shadow_count("Friend").expect("count"), 0);
    }

    #[test]
    fn unusable_logins_are_refused() {
        let scratch = Scratch::new("follow-bad");
        let mut store = Store::open(&scratch.path()).expect("open");
        assert!(store.follow_user("").is_err());
        assert!(store.follow_user("-lead").is_err());
    }

    #[test]
    fn refresh_replaces_and_stamps_the_follow() {
        let scratch = Scratch::new("refresh");
        let mut store = Store::open(&scratch.path()).expect("open");
        store.follow_user("friend").expect("follow");
        store
            .store_shadows(
                "friend",
                &[
                    shadow_snapshot("a", "one", 1),
                    shadow_snapshot("a", "two", 2),
                ],
                at(2026, 10, 29),
            )
            .expect("first");
        store
            .store_shadows(
                "friend",
                &[shadow_snapshot("a", "three", 3)],
                at(2026, 10, 30),
            )
            .expect("second");
        let shadows = store.shadows_for("friend").expect("shadows");
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].snapshot.name, "three");
        assert_eq!(shadows[0].fetched_at, at(2026, 10, 30));
        assert_eq!(
            store.followed_stamp("friend").expect("stamp"),
            Some(at(2026, 10, 30))
        );
    }

    #[test]
    fn shadows_sort_most_stars_first() {
        let scratch = Scratch::new("shadow-sort");
        let mut store = Store::open(&scratch.path()).expect("open");
        store
            .store_shadows(
                "friend",
                &[
                    shadow_snapshot("a", "small", 3),
                    shadow_snapshot("a", "big", 300),
                    shadow_snapshot("a", "mid", 30),
                ],
                at(2026, 10, 30),
            )
            .expect("store");
        let names: Vec<String> = store
            .shadows_for("friend")
            .expect("shadows")
            .into_iter()
            .map(|shadow| shadow.snapshot.name)
            .collect();
        assert_eq!(names, ["big", "mid", "small"]);
    }

    #[test]
    fn invariant_shadows_are_invisible_to_the_catalog_surfaces() {
        let scratch = Scratch::new("shadow-isolation");
        let mut store = Store::open(&scratch.path()).expect("open");
        let catalogued = sample_snapshot("someone", "catalogued");
        store
            .upsert_snapshots(std::slice::from_ref(&catalogued), at(2026, 10, 30))
            .expect("catalogue");
        let shadowed = shadow_snapshot("someone", "catalogued", 9_999);
        store
            .store_shadows("friend", &[shadowed], at(2026, 10, 30))
            .expect("shadow");
        let page = store
            .page(SortKey::Stars, 0, 500, &CatalogFilter::empty())
            .expect("page");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].stars, 10);
        assert_eq!(
            store.total_count(&CatalogFilter::empty()).expect("total"),
            1
        );
        let index = store.search_index_rows().expect("index");
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].name, "catalogued");
        let overview = store.overview().expect("overview");
        assert_eq!(overview.repos_present, 1);
        assert_eq!(overview.stars_total, 10);
        let shadowed_rows = store.shadows_for("friend").expect("shadows");
        assert_eq!(shadowed_rows.len(), 1);
        assert_eq!(shadowed_rows[0].snapshot.stars, 9_999);
    }
}
