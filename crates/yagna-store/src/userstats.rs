//! The user tab's aggregates: every number the USER tab shows is one
//! SQL aggregate over the catalog — no pipelines, no fetches.
//!
//! Stars-by-language attributes each repository's stars to its primary
//! language (the language the row shows); the language's own color
//! comes from the stored `languages_json` entry of any repository with
//! that primary language, so the bars use the languages' own colors.
//! Health buckets follow the 12-point index's tiers: 10–12, 7–9, 4–6,
//! 0–3. Stars-per-year reads `starred_at`; top tags read the user's
//! tags, top topics the sweep-carried topic lists.
//!
//! Items: `LanguageStars` — one language's stars, repositories and
//! color; `HealthBuckets` — the four tiers' counts; `FitSpread` — the
//! fit score's four quarters plus the unscored; `YearCount` — one
//! year's starred count; `NameCount` — one tag's or one topic's count.
//! Depends on: rusqlite (the queries), serde_json (nothing — the JSON
//! functions live in SQLite).
//! Called by: crate::lib (the Store facade) and the app crate's USER
//! tab.

use crate::{Store, StoreError};

/// One language's stars across the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageStars {
    pub name: String,
    pub stars: u64,
    pub repos: u64,
    /// The language's own color, as GitHub sent it — `#rrggbb`.
    pub color: Option<String>,
}

/// The health tiers' counts: 10–12, 7–9, 4–6, 0–3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HealthBuckets {
    pub strong: u64,
    pub solid: u64,
    pub weak: u64,
    pub poor: u64,
}

/// The fit score's spread across the catalog: how many scored rows sit
/// in each quarter of their own possible score, and how many present
/// rows carry no fit score at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FitSpread {
    /// 75–100% of the row's own maximum.
    pub strong: u64,
    /// 50–75%.
    pub solid: u64,
    /// 25–50%.
    pub weak: u64,
    /// Below 25%.
    pub poor: u64,
    /// Present rows with no fit score stored yet.
    pub unscored: u64,
}

/// Which quarter of its own maximum a fit total reaches — or nothing
/// stored at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FitTier {
    Strong,
    Solid,
    Weak,
    Poor,
    Unscored,
}

impl FitSpread {
    fn add(&mut self, tier: FitTier) {
        match tier {
            FitTier::Strong => self.strong += 1,
            FitTier::Solid => self.solid += 1,
            FitTier::Weak => self.weak += 1,
            FitTier::Poor => self.poor += 1,
            FitTier::Unscored => self.unscored += 1,
        }
    }
}

/// One fit total as a fraction of its maximum; a zero maximum reads as
/// zero fill, never as a full one.
fn fit_fraction(total: f64, maximum: f64) -> f64 {
    if maximum > 0.0 { total / maximum } else { 0.0 }
}

/// The quarter a fraction lands in.
fn fit_tier(fraction: f64) -> FitTier {
    match (fraction >= 0.75, fraction >= 0.5, fraction >= 0.25) {
        (true, _, _) => FitTier::Strong,
        (false, true, _) => FitTier::Solid,
        (false, false, true) => FitTier::Weak,
        (false, false, false) => FitTier::Poor,
    }
}

/// The maximum out of one stored fit breakdown. An unreadable breakdown
/// reads as zero maximum — the row lands in `poor`, never silently in
/// `strong`.
fn fit_maximum(json: &str) -> f64 {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| value.get("maximum").and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0)
}

/// One year's starred count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YearCount {
    pub year: String,
    pub repos: u64,
}

/// One named count: a tag's or a topic's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCount {
    pub name: String,
    pub count: u64,
}

impl Store {
    /// Stars by primary language, heaviest first: each repository's
    /// stars land on its primary language, with the language's own
    /// color where the catalog ever saw one.
    pub fn stats_by_language(&self, limit: u32) -> Result<Vec<LanguageStars>, StoreError> {
        let mut statement = self.conn.prepare(
            "WITH langs AS (
               SELECT primary_language AS name, SUM(stars) AS stars, COUNT(*) AS repos
               FROM repos
               WHERE missing_at IS NULL AND primary_language IS NOT NULL
               GROUP BY primary_language
             )
             SELECT langs.name, langs.stars, langs.repos,
               (SELECT json_extract(je.value, '$.color')
                FROM repos r, json_each(r.languages_json) je
                WHERE r.primary_language = langs.name
                  AND json_extract(je.value, '$.name') = langs.name
                  AND json_extract(je.value, '$.color') IS NOT NULL
                LIMIT 1) AS color
             FROM langs
             ORDER BY langs.stars DESC, langs.name ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            let stars: i64 = row.get(1)?;
            let repos: i64 = row.get(2)?;
            Ok(LanguageStars {
                name: row.get(0)?,
                stars: stars.max(0) as u64,
                repos: repos.max(0) as u64,
                color: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// The health tiers' counts over the scored, present repositories.
    pub fn stats_health_buckets(&self) -> Result<HealthBuckets, StoreError> {
        self.conn
            .query_row(HEALTH_BUCKETS, [], |row| {
                let bucket = |index: usize| -> u64 {
                    let value: i64 = row.get(index).unwrap_or(0);
                    value.max(0) as u64
                };
                Ok(HealthBuckets {
                    strong: bucket(0),
                    solid: bucket(1),
                    weak: bucket(2),
                    poor: bucket(3),
                })
            })
            .map_err(StoreError::from)
    }

    /// The fit tiers' counts: every present row lands in the quarter of
    /// its own maximum its total reaches — or in `unscored` when no fit
    /// score is stored yet.
    pub fn stats_fit_buckets(&self) -> Result<FitSpread, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT fit_score, fit_json FROM repos WHERE missing_at IS NULL")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Option<f64>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?;
        let mut spread = FitSpread::default();
        for row in rows {
            let (score, json) = row?;
            let tier = score.map_or(FitTier::Unscored, |total| {
                fit_tier(fit_fraction(
                    total,
                    fit_maximum(json.as_deref().unwrap_or("")),
                ))
            });
            spread.add(tier);
        }
        Ok(spread)
    }

    /// Stars-per-year: the year each `starred_at` falls in, oldest
    /// year first.
    pub fn stats_per_year(&self) -> Result<Vec<YearCount>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT substr(starred_at, 1, 4) AS year, COUNT(*)
             FROM repos
             WHERE starred_at IS NOT NULL AND missing_at IS NULL
             GROUP BY year
             ORDER BY year ASC",
        )?;
        let rows = statement.query_map([], |row| {
            let repos: i64 = row.get(1)?;
            Ok(YearCount {
                year: row.get(0)?,
                repos: repos.max(0) as u64,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// The user's most-carried tags, heaviest first.
    pub fn stats_top_tags(&self, limit: u32) -> Result<Vec<NameCount>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.name, COUNT(*) AS carries
             FROM tags t
             JOIN repo_tags rt ON rt.tag_id = t.id
             JOIN repos r ON r.id = rt.repo_rowid AND r.missing_at IS NULL
             WHERE t.kind = 'tag'
             GROUP BY t.id
             ORDER BY carries DESC, t.name ASC
             LIMIT ?1",
        )?;
        query_name_counts(&mut statement, [limit])
    }

    /// The catalog's most-carried topics, heaviest first.
    pub fn stats_top_topics(&self, limit: u32) -> Result<Vec<NameCount>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT je.value AS topic, COUNT(*) AS carries
             FROM repos r, json_each(r.topics_json) je
             WHERE r.missing_at IS NULL AND r.topics_json IS NOT NULL
             GROUP BY topic
             ORDER BY carries DESC, topic ASC
             LIMIT ?1",
        )?;
        query_name_counts(&mut statement, [limit])
    }
}

/// One aggregated `(name, count)` query into `NameCount`s.
fn query_name_counts(
    statement: &mut rusqlite::Statement<'_>,
    limit: [u32; 1],
) -> Result<Vec<NameCount>, StoreError> {
    let rows = statement.query_map(limit, |row| {
        let count: i64 = row.get(1)?;
        Ok(NameCount {
            name: row.get(0)?,
            count: count.max(0) as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

const HEALTH_BUCKETS: &str = "
SELECT
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NULL AND health_score BETWEEN 10 AND 12),
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NULL AND health_score BETWEEN 7 AND 9),
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NULL AND health_score BETWEEN 4 AND 6),
  (SELECT COUNT(*) FROM repos WHERE missing_at IS NULL AND health_score BETWEEN 0 AND 3)
";

#[cfg(test)]
mod tests {
    use crate::Store;
    use crate::TagKind;
    use crate::testing::Scratch;
    use chrono::TimeZone;
    use chrono::Utc;
    use yagna_core::model::{LanguageStat, RepoSnapshot};

    /// A snapshot with the fields the stats read: stars, primary
    /// language (the first language), its color, topics, starred date.
    fn snapshot(
        owner: &str,
        name: &str,
        stars: u64,
        language: Option<&str>,
        color: Option<&str>,
        starred_at: chrono::DateTime<Utc>,
    ) -> RepoSnapshot {
        let mut snap = crate::testing::sample_snapshot(owner, name);
        snap.stars = stars;
        snap.languages = language
            .map(|name| {
                vec![LanguageStat {
                    name: name.to_string(),
                    bytes: 1000,
                    color: color.map(|color| color.to_string()),
                }]
            })
            .unwrap_or_default();
        snap.starred_at = Some(starred_at);
        snap
    }

    fn store_with_snapshots(owner: &str, snaps: &[RepoSnapshot]) -> (Scratch, Store) {
        let scratch = Scratch::new(owner);
        let mut store = Store::open(&scratch.path()).expect("open");
        store.upsert_snapshots(snaps, Utc::now()).expect("seed");
        (scratch, store)
    }

    /// Stars land on the primary language; the color comes from the
    /// stored languages' own color; unscored and missing rows are
    /// excluded.
    #[test]
    fn stars_by_language_aggregates_colors_and_excludes_missing() {
        let at = |year: i32| {
            Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0)
                .single()
                .expect("date")
        };
        let snaps = vec![
            snapshot("o", "a", 100, Some("Rust"), Some("#dea584"), at(2024)),
            snapshot("o", "b", 50, Some("Rust"), None, at(2025)),
            snapshot("o", "c", 20, Some("Go"), Some("#00add8"), at(2026)),
            snapshot("o", "d", 5, None, None, at(2026)),
        ];
        let (_scratch, mut store) = store_with_snapshots("lang", &snaps);
        store
            .mark_missing(
                &[
                    ("o".into(), "a".into()),
                    ("o".into(), "b".into()),
                    ("o".into(), "d".into()),
                ],
                Utc::now(),
            )
            .expect("missing");
        let langs = store.stats_by_language(10).expect("stats");
        assert_eq!(langs.len(), 1, "go is missing, none has no language");
        assert_eq!(langs[0].name, "Rust");
        assert_eq!(langs[0].stars, 150);
        assert_eq!(langs[0].repos, 2);
        assert_eq!(langs[0].color.as_deref(), Some("#dea584"));
    }

    /// The health buckets follow the 12-point index's tiers. The
    /// scores are stamped directly: the tier math, not the health
    /// model, is what this query pins.
    #[test]
    fn health_buckets_count_the_four_tiers() {
        let snaps: Vec<RepoSnapshot> = (0..7)
            .map(|index| snapshot("o", &format!("r{index}"), 1, None, None, Utc::now()))
            .collect();
        let (_scratch, store) = store_with_snapshots("health", &snaps);
        for (index, score) in [11i64, 10, 9, 7, 5, 2, 0].into_iter().enumerate() {
            store
                .conn
                .execute(
                    "UPDATE repos SET health_score = ?1 WHERE name = ?2",
                    rusqlite::params![score, format!("r{index}")],
                )
                .expect("stamp");
        }
        let buckets = store.stats_health_buckets().expect("stats");
        assert_eq!(buckets.strong, 2, "10 and 11");
        assert_eq!(buckets.solid, 2, "7 and 9");
        assert_eq!(buckets.weak, 1, "5");
        assert_eq!(buckets.poor, 2, "0 and 2");
    }

    /// Stars-per-year groups by the starred year, oldest first.
    #[test]
    fn stars_per_year_group_by_year_in_order() {
        let at = |year: i32, month: u32| {
            Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
                .single()
                .expect("date")
        };
        let snaps = vec![
            snapshot("o", "a", 1, None, None, at(2023, 6)),
            snapshot("o", "b", 1, None, None, at(2024, 2)),
            snapshot("o", "c", 1, None, None, at(2024, 9)),
            snapshot("o", "d", 1, None, None, at(2026, 1)),
        ];
        let (_scratch, mut store) = store_with_snapshots("years", &snaps);
        store
            .mark_missing(
                &[
                    ("o".into(), "a".into()),
                    ("o".into(), "b".into()),
                    ("o".into(), "c".into()),
                ],
                Utc::now(),
            )
            .expect("missing");
        let years = store.stats_per_year().expect("stats");
        assert_eq!(years.len(), 2);
        assert_eq!(years[0].year, "2023");
        assert_eq!(years[0].repos, 1);
        assert_eq!(years[1].year, "2024");
        assert_eq!(years[1].repos, 2);
    }

    /// Top tags count present repositories only, heaviest first; the
    /// limit holds.
    #[test]
    fn top_tags_count_present_repositories_heaviest_first() {
        let snaps: Vec<RepoSnapshot> = (0..3)
            .map(|index| snapshot("o", &format!("r{index}"), 1, None, None, Utc::now()))
            .collect();
        let (_scratch, mut store) = store_with_snapshots("tags", &snaps);
        let seen: Vec<(String, String)> = (0..3)
            .map(|index| ("o".to_string(), format!("r{index}")))
            .collect();
        store.mark_missing(&seen, Utc::now()).expect("missing");
        // The fresh catalog's rowids run 1..=3, one per seeded repo.
        store.assign_tag(1, "cli", Some(TagKind::Tag)).expect("tag");
        store.assign_tag(2, "cli", Some(TagKind::Tag)).expect("tag");
        store.assign_tag(3, "cli", Some(TagKind::Tag)).expect("tag");
        store
            .assign_tag(1, "editor", Some(TagKind::Tag))
            .expect("tag");
        store
            .assign_tag(2, "ui-kit", Some(TagKind::Category))
            .expect("category");
        let top = store.stats_top_tags(10).expect("stats");
        assert_eq!(top.len(), 2, "categories are not tags");
        assert_eq!(top[0].name, "cli");
        assert_eq!(top[0].count, 3);
        assert_eq!(top[1].name, "editor");
        assert_eq!(top[1].count, 1);
        assert_eq!(
            store.stats_top_tags(1).expect("stats").len(),
            1,
            "the limit holds"
        );
    }

    /// Top topics count topic occurrences across present repositories.
    #[test]
    fn top_topics_count_topic_occurrences() {
        let mut a = snapshot("o", "a", 1, None, None, Utc::now());
        a.topics = vec!["rust".into(), "cli".into()];
        let mut b = snapshot("o", "b", 1, None, None, Utc::now());
        b.topics = vec!["rust".into()];
        let (_scratch, store) = store_with_snapshots("topics", &[a, b]);
        let top = store.stats_top_topics(10).expect("stats");
        assert_eq!(top[0].name, "rust");
        assert_eq!(top[0].count, 2);
        assert_eq!(top[1].name, "cli");
        assert_eq!(top[1].count, 1);
    }
}
