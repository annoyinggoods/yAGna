//! The 12-point repository health index: pure scoring, no side effects.
//!
//! `health` scores one `RepoSnapshot` against a caller-supplied `now`.
//! The criteria are exclusive tiers: age 3/2/0, stars 2/1/0, maintenance
//! 2/0, cadence 1/0, license 2/0, version 2/0. Every boundary is pinned by
//! an `invariant_` test that may only ever be strengthened.
//!
//! Items: `health` — the scorer; `HealthReport` — total plus breakdown;
//! `PERMISSIVE_LICENSES` — the SPDX ids that earn license points.
//! Depends on: crate::model (the snapshot it scores), chrono (calendar
//! months).
//! Called by: the store (score storage) and the app (the detail view).

use chrono::{DateTime, Months, Utc};

use crate::model::RepoSnapshot;

/// The SPDX ids that earn full license points: permissive only. Copyleft
/// (the GPL family, MPL, EPL) and source-available texts score zero here —
/// what a non-standard license actually says is a text-reading concern,
/// not a number in this index.
pub const PERMISSIVE_LICENSES: [&str; 8] = [
    "MIT",
    "Apache-2.0",
    "0BSD",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unlicense",
    "Zlib",
];

/// The score of one repository: the 0–12 total plus the per-criterion
/// points behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HealthReport {
    pub total: u8,
    pub age: u8,
    pub stars: u8,
    pub maintenance: u8,
    pub cadence: u8,
    pub license: u8,
    pub version: u8,
}

impl HealthReport {
    /// The report's breakdown: one row per criterion — its name, the
    /// points it scored, and the points it could score. The detail view
    /// renders this table; the index itself lives here.
    pub fn breakdown(&self) -> Vec<(&'static str, u8, u8)> {
        vec![
            ("age", self.age, 3),
            ("stars", self.stars, 2),
            ("maintenance", self.maintenance, 2),
            ("cadence", self.cadence, 1),
            ("license", self.license, 2),
            ("version", self.version, 2),
        ]
    }
}

/// Scores a snapshot against `now`. `now` is a parameter, never read from
/// a clock, so the same snapshot always scores the same.
pub fn health(snapshot: &RepoSnapshot, now: DateTime<Utc>) -> HealthReport {
    let age = age_points(snapshot.created_at, now);
    let stars = star_points(snapshot.stars);
    let maintenance = maintenance_points(snapshot.created_at, snapshot.last_commit_at, now);
    let cadence = cadence_points(snapshot.commit_count, snapshot.created_at, now);
    let license = license_points(
        snapshot
            .license
            .as_ref()
            .and_then(|info| info.spdx_id.as_deref()),
    );
    let version = version_points(
        snapshot
            .latest_release
            .as_ref()
            .map(|release| release.tag.as_str()),
    );
    let total = age + stars + maintenance + cadence + license + version;
    HealthReport {
        total,
        age,
        stars,
        maintenance,
        cadence,
        license,
        version,
    }
}

/// Age tiers: strictly older than 2 years → 3, strictly older than 1
/// year → 2, else 0. "Older" compares against calendar-month cutoffs, so
/// a repo exactly 24 months old sits in the 1-year tier.
fn age_points(created: DateTime<Utc>, now: DateTime<Utc>) -> u8 {
    if older_than(created, now, 24) {
        return 3;
    }
    if older_than(created, now, 12) {
        return 2;
    }
    0
}

/// Star tiers: at least 1000 → 2, at least 100 → 1, else 0.
fn star_points(stars: u64) -> u8 {
    if stars >= 1000 {
        return 2;
    }
    if stars >= 100 {
        return 1;
    }
    0
}

/// Maintenance: 2 points for the "old but still maintained" shape — the
/// repo is older than 18 months and its last commit lands within 10
/// months of `now` (the boundary instant counts as within). A young repo
/// scores 0 here: its age already says it is alive.
fn maintenance_points(
    created: DateTime<Utc>,
    last_commit: DateTime<Utc>,
    now: DateTime<Utc>,
) -> u8 {
    if !older_than(created, now, 18) {
        return 0;
    }
    if commit_within(last_commit, now, 10) {
        return 2;
    }
    0
}

/// Cadence: 1 point when commits per year of age exceed 5. A repo with no
/// commits scores 0; a repo younger than a day — or with an inverted age —
/// is measured over one day.
fn cadence_points(commits: u64, created: DateTime<Utc>, now: DateTime<Utc>) -> u8 {
    if commits == 0 {
        return 0;
    }
    let days = (now - created).num_days().max(1) as f64;
    if commits as f64 / (days / 365.25) > 5.0 {
        return 1;
    }
    0
}

/// License: 2 points when the SPDX id is exactly in the permissive set.
/// A missing license, or a license without an SPDX id, scores 0.
fn license_points(spdx_id: Option<&str>) -> u8 {
    match spdx_id {
        Some(id) if PERMISSIVE_LICENSES.contains(&id) => 2,
        _ => 0,
    }
}

/// Version: 2 points when the latest release tag parses as a semver at
/// least 1.0.0. A leading `v` is allowed; pre-release and build suffixes
/// are ignored; anything unparseable or below 1.0.0 scores 0.
fn version_points(tag: Option<&str>) -> u8 {
    match tag.and_then(parse_semver) {
        Some((major, _, _)) if major >= 1 => 2,
        _ => 0,
    }
}

/// True when `created` is strictly before `now` minus `months` calendar
/// months. A cutoff that cannot be represented means "ancient": older.
fn older_than(created: DateTime<Utc>, now: DateTime<Utc>, months: u32) -> bool {
    match months_before(now, months) {
        Some(cutoff) => created < cutoff,
        None => true,
    }
}

/// True when `at` lands within `months` of `now` — at or after `now`
/// minus that many months. An unrepresentable cutoff means the window is
/// unbounded: within.
fn commit_within(at: DateTime<Utc>, now: DateTime<Utc>, months: u32) -> bool {
    match months_before(now, months) {
        Some(cutoff) => at >= cutoff,
        None => true,
    }
}

/// `now` minus `months` calendar months, or none when the result has no
/// representable date.
fn months_before(now: DateTime<Utc>, months: u32) -> Option<DateTime<Utc>> {
    let naive = now.naive_utc().checked_sub_months(Months::new(months))?;
    Some(DateTime::from_naive_utc_and_offset(naive, Utc))
}

/// Parses the leading `v`-optional MAJOR.MINOR.PATCH out of a tag, cutting
/// any pre-release or build suffix. All three components must be present
/// and numeric; a fourth dotted component is rejected.
pub fn parse_semver(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    let numbers = core.split(['-', '+']).next()?;
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Forge, LicenseInfo, ReleaseInfo};

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn now() -> DateTime<Utc> {
        date(2026, 6, 15)
    }

    fn snapshot() -> RepoSnapshot {
        RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: "owner".to_string(),
            name: "repo".to_string(),
            description: None,
            stars: 0,
            languages: vec![],
            license: None,
            topics: vec![],
            default_branch: None,
            created_at: date(2000, 1, 1),
            last_commit_at: date(2000, 1, 1),
            starred_at: None,
            commit_count: 0,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    #[test]
    fn invariant_age_tiers_pin_the_year_edges() {
        let mut repo = snapshot();
        repo.created_at = date(2024, 6, 14);
        assert_eq!(health(&repo, now()).age, 3);
        repo.created_at = date(2024, 6, 15);
        assert_eq!(
            health(&repo, now()).age,
            2,
            "exactly 24 months is not 'older than 2 years'"
        );
        repo.created_at = date(2025, 6, 14);
        assert_eq!(health(&repo, now()).age, 2);
        repo.created_at = date(2025, 6, 15);
        assert_eq!(
            health(&repo, now()).age,
            0,
            "exactly 12 months is not 'older than 1 year'"
        );
        repo.created_at = now();
        assert_eq!(health(&repo, now()).age, 0);
    }

    #[test]
    fn invariant_star_tiers_pin_the_100_and_1000_edges() {
        let mut repo = snapshot();
        for (stars, expected) in [(99u64, 0u8), (100, 1), (999, 1), (1000, 2)] {
            repo.stars = stars;
            assert_eq!(health(&repo, now()).stars, expected, "stars = {stars}");
        }
    }

    #[test]
    fn invariant_maintenance_pins_old_but_alive() {
        let mut repo = snapshot();
        repo.created_at = date(2024, 1, 1);
        repo.last_commit_at = date(2026, 6, 1);
        assert_eq!(health(&repo, now()).maintenance, 2);
        repo.last_commit_at = date(2025, 8, 15);
        assert_eq!(
            health(&repo, now()).maintenance,
            2,
            "the boundary instant of the 10-month window counts as within"
        );
    }

    #[test]
    fn invariant_maintenance_pins_young_and_alive_and_old_and_dead() {
        let mut repo = snapshot();
        repo.created_at = date(2026, 1, 1);
        repo.last_commit_at = now();
        assert_eq!(
            health(&repo, now()).maintenance,
            0,
            "a young-and-alive repo scores 0: age says alive"
        );
        repo.created_at = date(2024, 1, 1);
        repo.last_commit_at = date(2025, 8, 14);
        assert_eq!(
            health(&repo, now()).maintenance,
            0,
            "an old-and-dead repo scores 0"
        );
    }

    #[test]
    fn invariant_cadence_pins_the_division() {
        let mut repo = snapshot();
        repo.created_at = date(2000, 1, 1);
        repo.commit_count = 0;
        assert_eq!(
            health(&repo, now()).cadence,
            0,
            "zero commits never score, whatever the age"
        );
        repo.created_at = date(2006, 6, 15);
        repo.commit_count = 100;
        assert_eq!(
            health(&repo, now()).cadence,
            0,
            "100 commits over exactly 20 years is 5 per year, not above 5"
        );
        repo.commit_count = 101;
        assert_eq!(health(&repo, now()).cadence, 1);
        repo.created_at = date(2026, 3, 15);
        repo.commit_count = 100;
        assert_eq!(
            health(&repo, now()).cadence,
            1,
            "a partial year still divides: 100 commits in 3 months"
        );
        repo.created_at = now();
        repo.commit_count = 3;
        assert_eq!(
            health(&repo, now()).cadence,
            1,
            "a brand-new repo is measured over one day"
        );
    }

    #[test]
    fn invariant_license_classes_pin_the_permissive_set() {
        let mut repo = snapshot();
        for spdx in PERMISSIVE_LICENSES {
            repo.license = Some(LicenseInfo {
                name: "some license".to_string(),
                spdx_id: Some(spdx.to_string()),
            });
            assert_eq!(health(&repo, now()).license, 2, "spdx = {spdx}");
        }
        for spdx in [
            "GPL-3.0-only",
            "LGPL-3.0-only",
            "AGPL-3.0-only",
            "MPL-2.0",
            "EPL-2.0",
            "BSL-1.0",
            "Hippocratic-2.1",
        ] {
            repo.license = Some(LicenseInfo {
                name: "some license".to_string(),
                spdx_id: Some(spdx.to_string()),
            });
            assert_eq!(health(&repo, now()).license, 0, "spdx = {spdx}");
        }
        repo.license = Some(LicenseInfo {
            name: "Custom".to_string(),
            spdx_id: None,
        });
        assert_eq!(health(&repo, now()).license, 0, "no SPDX id scores 0");
        repo.license = None;
        assert_eq!(health(&repo, now()).license, 0, "no license scores 0");
    }

    #[test]
    fn invariant_version_pins_the_semver_edge() {
        let mut repo = snapshot();
        for (tag, expected) in [
            ("0.9.9", 0u8),
            ("1.0.0", 2),
            ("v1.0.0", 2),
            ("v2.3.4", 2),
            ("1.0.0-rc.1", 2),
            ("0.9.0-rc.1", 0),
            ("not-a-version", 0),
            ("1.2", 0),
            ("", 0),
        ] {
            repo.latest_release = Some(ReleaseInfo {
                tag: tag.to_string(),
                published_at: None,
            });
            assert_eq!(health(&repo, now()).version, expected, "tag = {tag:?}");
        }
        repo.latest_release = None;
        assert_eq!(
            health(&repo, now()).version,
            0,
            "a missing release scores 0"
        );
    }

    #[test]
    fn invariant_total_caps_at_twelve_and_floors_at_zero() {
        let mut repo = snapshot();
        repo.created_at = date(2010, 1, 1);
        repo.last_commit_at = now();
        repo.stars = 5_000;
        repo.commit_count = 100_000;
        repo.license = Some(LicenseInfo {
            name: "MIT".to_string(),
            spdx_id: Some("MIT".to_string()),
        });
        repo.latest_release = Some(ReleaseInfo {
            tag: "v2.0.0".to_string(),
            published_at: None,
        });
        assert_eq!(health(&repo, now()).total, 12);
        let empty = RepoSnapshot {
            created_at: now(),
            last_commit_at: now(),
            ..snapshot()
        };
        assert_eq!(health(&empty, now()).total, 0);
    }

    #[test]
    fn health_report_roundtrips_through_json() {
        let report = health(&snapshot(), now());
        let text = serde_json::to_string(&report).expect("serialize");
        let back: HealthReport = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(report, back);
    }

    #[test]
    fn the_breakdown_names_every_criterion_and_its_maximum() {
        let report = HealthReport {
            total: 10,
            age: 3,
            stars: 2,
            maintenance: 2,
            cadence: 1,
            license: 2,
            version: 0,
        };
        let rows = report.breakdown();
        assert_eq!(rows.len(), 6);
        let names: Vec<&str> = rows.iter().map(|row| row.0).collect();
        assert_eq!(
            names,
            [
                "age",
                "stars",
                "maintenance",
                "cadence",
                "license",
                "version"
            ]
        );
        let maximum: u32 = rows.iter().map(|row| u32::from(row.2)).sum();
        assert_eq!(maximum, 12, "the criteria add up to the index's twelve");
        let scored: u32 = rows.iter().map(|row| u32::from(row.1)).sum();
        assert_eq!(scored, u32::from(report.total), "the rows are the total");
    }
}
