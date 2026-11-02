//! The export format: the JSON shape the catalog is written out in and
//! read back from.
//!
//! The shape is gh-stars-exporter's — `id`, `name`, `full_name`,
//! `html_url`, `description`, `created_at`, `pushed_at`,
//! `stargazers_count`, `language`, `topics`, `starred_at` — plus one
//! `yagna` object carrying what only this library knows: the user's
//! tags and categories, the stored health breakdown, the enrichment
//! facts a roundtrip must restore (the exact first commit), and the
//! repository's languages, license and release.
//!
//! Items: `ExportFile` — the file's shape: the format version, when
//! it was written, the repositories; `ExportRepo` — one repository in
//! the exporter's shape plus the `yagna` extension; `ExportExtension`
//! — the yagna-only facts; `ExportTag` — one tag or category with its
//! kind.
//! Depends on: serde (the wire format), chrono (instants),
//! yagna_core::model (the language, license and release records),
//! yagna_core::health (the health breakdown).
//! Called by: the store (which reads and writes these rows) and the
//! app crate's export and import verbs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::health::HealthReport;
use crate::model::{LanguageStat, LicenseInfo, ReleaseInfo};

/// The format version this build writes: bumped when the shape
/// changes in a way an older build could not read.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// The export file: what was written, when, and the repositories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportFile {
    /// The format version — `1` today.
    pub format_version: u32,
    /// When the export was taken.
    pub exported_at: DateTime<Utc>,
    /// Where it came from: always `"yagna"`.
    pub source: String,
    /// The repositories, in the catalog's own order.
    pub repos: Vec<ExportRepo>,
}

impl ExportFile {
    /// A file holding one export, written now.
    pub fn new(repos: Vec<ExportRepo>, now: DateTime<Utc>) -> Self {
        Self {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: now,
            source: "yagna".to_string(),
            repos,
        }
    }
}

/// One repository in the exporter's shape, plus the yagna extension.
///
/// The base fields are the ones every stars exporter writes: the
/// identity, the URLs, the description, the dates, the star count, the
/// primary language, the topics, and when the user starred it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportRepo {
    /// The service's own id for the repository.
    pub id: u64,
    /// The repository's name.
    pub name: String,
    /// `owner/name`.
    pub full_name: String,
    /// The repository's page on the service.
    pub html_url: String,
    /// The description, when it has one.
    #[serde(default)]
    pub description: Option<String>,
    /// When the repository was created.
    pub created_at: DateTime<Utc>,
    /// When it was last pushed to.
    pub pushed_at: DateTime<Utc>,
    /// The star count.
    #[serde(default)]
    pub stargazers_count: u64,
    /// The primary language, when it has one.
    #[serde(default)]
    pub language: Option<String>,
    /// The topics.
    #[serde(default)]
    pub topics: Vec<String>,
    /// When the user starred it.
    #[serde(default)]
    pub starred_at: Option<DateTime<Utc>>,
    /// Whether it is a fork.
    #[serde(default)]
    pub fork: bool,
    /// Whether it is archived.
    #[serde(default)]
    pub archived: bool,
    /// What only this library knows.
    #[serde(default)]
    pub yagna: ExportExtension,
}

/// The yagna-only facts: the library's own record of a repository.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportExtension {
    /// Which service hosts it: `github` today.
    #[serde(default)]
    pub forge: String,
    /// The owner, as the service spells it.
    #[serde(default)]
    pub owner: String,
    /// The default branch, when it has one.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// The language breakdown, by size.
    #[serde(default)]
    pub languages: Vec<LanguageStat>,
    /// The license, when the service named one.
    #[serde(default)]
    pub license: Option<LicenseInfo>,
    /// The commit count.
    #[serde(default)]
    pub commit_count: u64,
    /// The latest release, when it has one.
    #[serde(default)]
    pub latest_release: Option<ReleaseInfo>,
    /// The exact first commit's date, when it was fetched.
    #[serde(default)]
    pub first_commit_at: Option<DateTime<Utc>>,
    /// Where that date came from: `repo-created` or `git-history`.
    #[serde(default)]
    pub first_commit_source: Option<String>,
    /// Whether the repository went missing (the sweep no longer sees
    /// it) — the catalog is history, and the export keeps it.
    #[serde(default)]
    pub missing: bool,
    /// When it went missing, when it did — the "gone since" stamp
    /// travels so an import cannot replace history with its own clock.
    #[serde(default)]
    pub missing_at: Option<String>,
    /// The user's tags and categories.
    #[serde(default)]
    pub tags: Vec<ExportTag>,
    /// The stored health breakdown — the score and its six parts. It
    /// travels verbatim: health is a function of the snapshot and the
    /// clock, and an import's clock is not the export's.
    #[serde(default)]
    pub health: Option<HealthReport>,
}

/// One tag or category the user attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTag {
    /// The tag's name.
    pub name: String,
    /// Its kind: `tag` or `category`.
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    fn repo() -> ExportRepo {
        ExportRepo {
            id: 42,
            name: "something".to_string(),
            full_name: "someone/something".to_string(),
            html_url: "https://github.com/someone/something".to_string(),
            description: Some("a catalog".to_string()),
            created_at: date(2020, 1, 2),
            pushed_at: date(2026, 10, 1),
            stargazers_count: 1_234,
            language: Some("Rust".to_string()),
            topics: vec!["rust".to_string()],
            starred_at: Some(date(2026, 10, 20)),
            fork: false,
            archived: false,
            yagna: ExportExtension {
                forge: "github".to_string(),
                owner: "someone".to_string(),
                default_branch: Some("main".to_string()),
                languages: vec![LanguageStat {
                    name: "Rust".to_string(),
                    bytes: 500,
                    color: Some("#dea584".to_string()),
                }],
                license: Some(LicenseInfo {
                    name: "MIT License".to_string(),
                    spdx_id: Some("MIT".to_string()),
                }),
                commit_count: 500,
                latest_release: None,
                first_commit_at: Some(date(2020, 1, 2)),
                first_commit_source: Some("repo-created".to_string()),
                missing: false,
                missing_at: None,
                tags: vec![ExportTag {
                    name: "reading list".to_string(),
                    kind: "tag".to_string(),
                }],
                health: Some(HealthReport::default()),
            },
        }
    }

    #[test]
    fn an_export_file_roundtrips_through_json() {
        let file = ExportFile::new(vec![repo()], date(2026, 10, 30));
        let text = serde_json::to_string_pretty(&file).expect("serialize");
        let back: ExportFile = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(file, back);
    }

    #[test]
    fn the_exporter_shape_is_the_base_of_the_row() {
        let text = serde_json::to_string(&repo()).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        for field in [
            "id",
            "name",
            "full_name",
            "html_url",
            "description",
            "created_at",
            "pushed_at",
            "stargazers_count",
            "language",
            "topics",
            "starred_at",
        ] {
            assert!(value.get(field).is_some(), "the row carries {field}");
        }
        assert!(
            value.get("yagna").is_some(),
            "the yagna extension rides along"
        );
    }

    #[test]
    fn a_row_without_the_extension_still_reads() {
        let value = serde_json::json!({
            "id": 1,
            "name": "thing",
            "full_name": "someone/thing",
            "html_url": "https://example.com",
            "created_at": "2020-01-02T12:00:00Z",
            "pushed_at": "2026-10-01T12:00:00Z",
        });
        let row: ExportRepo = serde_json::from_value(value).expect("deserialize");
        assert_eq!(row.stargazers_count, 0);
        assert!(row.topics.is_empty());
        assert_eq!(row.yagna.owner, "", "the extension defaults empty");
        assert!(row.yagna.health.is_none());
    }
}
