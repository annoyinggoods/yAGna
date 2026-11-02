//! The spec-defined fit score beside the pinned repository health index.
//!
//! Items: `FitSpec` — versioned scoring data; `FitParameter` — one named
//! weighted rule; `FitRule` — the four rule kinds (`bands`, `match`,
//! `overlap`, `when_true`); `Band` — one numeric tier; `FitFacts` — a
//! repository's collected inputs; `FitReport` — the scoring spec's
//! version, the total, and the per-parameter breakdown;
//! `FitParamScore` — one parameter's contribution; `FitError` —
//! validation failures; `ParamKind` — the four fact kinds;
//! `PARAMETERS` — the closed 18-name vocabulary.
//! Core functions: `score` — deterministic scoring; `parse_spec` —
//! JSON parse plus catalogue validation; `default_spec` — the shipped
//! starting spec; `parameter_kind` — the kind of one catalogue name.
//! Depends on: serde/serde_json (the data-only spec), thiserror.
//! Called by: the store and app.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The closed vocabulary of parameter names the spec may use. A name
/// outside this list is refused at parse time.
pub const PARAMETERS: [&str; 18] = [
    "recency_days",
    "created_days",
    "stars",
    "commit_count",
    "commits_per_year",
    "release_age_days",
    "release_major",
    "license_class",
    "language",
    "topics",
    "health",
    "readme_present",
    "digest_present",
    "is_fork",
    "is_archived",
    "local",
    "tagged",
    "starred_days",
];

/// The four fact kinds the catalogue's parameters fall into. Each kind
/// has exactly one scoring rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Number,
    Category,
    Set,
    Boolean,
}

impl ParamKind {
    /// The rule spelling this kind's parameter must carry.
    fn rule_name(self) -> &'static str {
        match self {
            ParamKind::Number => "bands",
            ParamKind::Category => "match",
            ParamKind::Set => "overlap",
            ParamKind::Boolean => "when_true",
        }
    }
}

/// The kind of one catalogue parameter, or none when the name is not in
/// the catalogue.
pub fn parameter_kind(name: &str) -> Option<ParamKind> {
    match name {
        "recency_days" | "created_days" | "stars" | "commit_count" | "commits_per_year"
        | "release_age_days" | "release_major" | "health" | "starred_days" => {
            Some(ParamKind::Number)
        }
        "license_class" | "language" => Some(ParamKind::Category),
        "topics" => Some(ParamKind::Set),
        "readme_present" | "digest_present" | "is_fork" | "is_archived" | "local" | "tagged" => {
            Some(ParamKind::Boolean)
        }
        _ => None,
    }
}

/// The scoring spec: a version, a human name, and the weighted parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitSpec {
    pub version: u32,
    pub name: String,
    pub parameters: Vec<FitParameter>,
}

/// One named parameter: its catalogue name, its weight (the maximum
/// contribution), and its scoring rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitParameter {
    pub name: String,
    pub weight: f64,
    #[serde(flatten)]
    pub rule: FitRule,
}

/// The four rule kinds, one per fact kind. The JSON shape flattens the
/// rule fields into the parameter object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FitRule {
    Bands {
        bands: Vec<Band>,
    },
    MatchRule {
        #[serde(rename = "match")]
        values: BTreeMap<String, f64>,
    },
    OverlapRule {
        overlap: f64,
        wanted: Vec<String>,
    },
    WhenTrueRule {
        when_true: f64,
    },
}

impl FitRule {
    /// The rule's spelling — the one its parameter's kind requires.
    fn kind_name(&self) -> &'static str {
        match self {
            FitRule::Bands { .. } => "bands",
            FitRule::MatchRule { .. } => "match",
            FitRule::OverlapRule { .. } => "overlap",
            FitRule::WhenTrueRule { .. } => "when_true",
        }
    }
}

/// One numeric tier: score `points` when the value is at or below `max`
/// (absent `max` catches everything the earlier bands missed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Band {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub points: f64,
}

/// A repository's collected facts the scorer reads. Absent optional
/// fields score zero for their parameter.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FitFacts {
    pub recency_days: Option<f64>,
    pub created_days: Option<f64>,
    pub stars: u64,
    pub commit_count: u64,
    pub commits_per_year: Option<f64>,
    pub release_age_days: Option<f64>,
    pub release_major: Option<u64>,
    pub license_class: Option<String>,
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub health: u8,
    pub readme_present: bool,
    pub digest_present: bool,
    pub is_fork: bool,
    pub is_archived: bool,
    pub local: bool,
    pub tagged: bool,
    pub starred_days: Option<f64>,
}

/// The score of one repository: the version of the spec that scored it,
/// the weighted total, the maximum possible, and each parameter's
/// contribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitReport {
    /// The spec's `version` at scoring time. The stored breakdowns
    /// carry it, so a reader can tell a row scored by version 2 from
    /// one scored by version 3; `0` marks a breakdown written before
    /// the version travelled with the report.
    #[serde(default)]
    pub version: u32,
    pub total: f64,
    pub maximum: f64,
    pub parameters: Vec<FitParamScore>,
}

/// One parameter's contribution to a fit score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitParamScore {
    pub name: String,
    pub weight: f64,
    pub points: f64,
}

/// The errors the spec parser can raise.
#[derive(Debug, thiserror::Error)]
pub enum FitError {
    #[error("fit spec JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("fit spec parameter `{0}` is not in the catalogue")]
    UnknownParameter(String),
    #[error("fit spec parameter `{0}` has no scoring rule")]
    MissingRule(String),
    #[error("fit spec parameter `{0}` carries a rule its kind does not allow")]
    RuleMismatch(String),
    #[error("fit spec parameter `{0}` carries a bad value")]
    BadValue(String),
}

/// Parses and validates a fit spec from its JSON text. Every parameter
/// name must be in the closed catalogue.
pub fn parse_spec(text: &str) -> Result<FitSpec, FitError> {
    let spec: FitSpec = serde_json::from_str(text)?;
    validate_spec(&spec)?;
    Ok(spec)
}

/// The shipped default: recency, licence class, health and local
/// presence — enough for the second score to mean something on first
/// launch.
pub fn default_spec() -> FitSpec {
    FitSpec {
        version: 1,
        name: "default fit".to_string(),
        parameters: vec![
            FitParameter {
                name: "recency_days".to_string(),
                weight: 3.0,
                rule: FitRule::Bands {
                    bands: vec![
                        Band {
                            max: Some(90.0),
                            points: 3.0,
                        },
                        Band {
                            max: Some(365.0),
                            points: 1.0,
                        },
                        Band {
                            max: None,
                            points: 0.0,
                        },
                    ],
                },
            },
            FitParameter {
                name: "license_class".to_string(),
                weight: 2.0,
                rule: FitRule::MatchRule {
                    values: BTreeMap::from([
                        ("permissive".to_string(), 2.0),
                        ("copyleft".to_string(), 0.0),
                        ("unknown".to_string(), 0.5),
                    ]),
                },
            },
            FitParameter {
                name: "health".to_string(),
                weight: 2.0,
                rule: FitRule::Bands {
                    bands: vec![
                        Band {
                            max: Some(4.0),
                            points: 0.0,
                        },
                        Band {
                            max: Some(8.0),
                            points: 1.0,
                        },
                        Band {
                            max: None,
                            points: 2.0,
                        },
                    ],
                },
            },
            FitParameter {
                name: "local".to_string(),
                weight: 0.5,
                rule: FitRule::WhenTrueRule { when_true: 0.5 },
            },
        ],
    }
}

/// Scores a repository's facts against a spec. Deterministic: the same
/// inputs always produce the same report. A rule that offers more than
/// its parameter's weight is capped there — the weight is the maximum
/// contribution, so no report's total exceeds its maximum.
pub fn score(spec: &FitSpec, facts: &FitFacts) -> FitReport {
    let parameters: Vec<FitParamScore> = spec
        .parameters
        .iter()
        .map(|param| score_parameter(param, facts))
        .collect();
    let total = parameters.iter().map(|p| p.points).sum();
    let maximum = parameters.iter().map(|p| p.weight).sum();
    FitReport {
        version: spec.version,
        total,
        maximum,
        parameters,
    }
}

/// Validates every parameter name against the closed catalogue, its
/// rule against its kind, and its numbers for sanity.
fn validate_spec(spec: &FitSpec) -> Result<(), FitError> {
    for param in &spec.parameters {
        let Some(kind) = parameter_kind(&param.name) else {
            return Err(FitError::UnknownParameter(param.name.clone()));
        };
        if param.rule.kind_name() != kind.rule_name() {
            return Err(FitError::RuleMismatch(param.name.clone()));
        }
        if !param.weight.is_finite() || param.weight < 0.0 {
            return Err(FitError::BadValue(param.name.clone()));
        }
        validate_rule_numbers(&param.name, &param.rule)?;
    }
    Ok(())
}

/// The numbers inside one rule: finite and non-negative.
fn validate_rule_numbers(name: &str, rule: &FitRule) -> Result<(), FitError> {
    let bad = |found: f64| !found.is_finite() || found < 0.0;
    match rule {
        FitRule::Bands { bands } => {
            if bands.is_empty() || bands.iter().any(|band| bad(band.points)) {
                return Err(FitError::BadValue(name.to_string()));
            }
        }
        FitRule::MatchRule { values } => {
            if values.is_empty() || values.values().copied().any(bad) {
                return Err(FitError::BadValue(name.to_string()));
            }
        }
        FitRule::OverlapRule { overlap, wanted } => {
            if bad(*overlap) || wanted.is_empty() {
                return Err(FitError::BadValue(name.to_string()));
            }
        }
        FitRule::WhenTrueRule { when_true } => {
            if bad(*when_true) {
                return Err(FitError::BadValue(name.to_string()));
            }
        }
    }
    Ok(())
}

/// Scores one parameter against the facts, capped at its weight.
fn score_parameter(param: &FitParameter, facts: &FitFacts) -> FitParamScore {
    let value = fact_value(&param.name, facts);
    let points = apply_rule(&param.rule, &value).min(param.weight);
    FitParamScore {
        name: param.name.clone(),
        weight: param.weight,
        points,
    }
}

/// The internal fact representation the rules consume.
#[derive(Debug, Clone)]
enum FactValue {
    Number(f64),
    Category(String),
    Set(Vec<String>),
    Boolean(bool),
    Missing,
}

/// Reads one named fact from the repository's inputs.
fn fact_value(name: &str, facts: &FitFacts) -> FactValue {
    match name {
        "recency_days" => number_fact(facts.recency_days),
        "created_days" => number_fact(facts.created_days),
        "stars" => FactValue::Number(facts.stars as f64),
        "commit_count" => FactValue::Number(facts.commit_count as f64),
        "commits_per_year" => number_fact(facts.commits_per_year),
        "release_age_days" => number_fact(facts.release_age_days),
        "release_major" => number_fact(facts.release_major.map(|v| v as f64)),
        "license_class" => category_fact(facts.license_class.as_deref()),
        "language" => category_fact(facts.language.as_deref()),
        "topics" => FactValue::Set(facts.topics.clone()),
        "health" => FactValue::Number(facts.health as f64),
        "readme_present" => FactValue::Boolean(facts.readme_present),
        "digest_present" => FactValue::Boolean(facts.digest_present),
        "is_fork" => FactValue::Boolean(facts.is_fork),
        "is_archived" => FactValue::Boolean(facts.is_archived),
        "local" => FactValue::Boolean(facts.local),
        "tagged" => FactValue::Boolean(facts.tagged),
        "starred_days" => number_fact(facts.starred_days),
        _ => FactValue::Missing,
    }
}

/// Wraps an optional number as a fact value.
fn number_fact(value: Option<f64>) -> FactValue {
    value.map_or(FactValue::Missing, FactValue::Number)
}

/// Wraps an optional category as a fact value.
fn category_fact(value: Option<&str>) -> FactValue {
    value.map_or(FactValue::Missing, |s| FactValue::Category(s.to_string()))
}

/// Applies one rule to one fact value.
fn apply_rule(rule: &FitRule, value: &FactValue) -> f64 {
    match rule {
        FitRule::Bands { bands } => apply_bands(bands, value),
        FitRule::MatchRule { values } => apply_match(values, value),
        FitRule::OverlapRule { overlap, wanted } => apply_overlap(wanted, *overlap, value),
        FitRule::WhenTrueRule { when_true } => apply_when_true(*when_true, value),
    }
}

/// Bands: first tier whose `max` is at or above the value wins; an
/// unbounded tier catches the rest. Non-numeric facts score zero.
fn apply_bands(bands: &[Band], value: &FactValue) -> f64 {
    let FactValue::Number(n) = value else {
        return 0.0;
    };
    bands
        .iter()
        .find(|band| band.max.is_none_or(|max| *n <= max))
        .map_or(0.0, |band| band.points)
}

/// Match: look up the category in the map. Non-category facts score zero.
fn apply_match(values: &BTreeMap<String, f64>, value: &FactValue) -> f64 {
    let FactValue::Category(cat) = value else {
        return 0.0;
    };
    values.get(cat.as_str()).copied().unwrap_or(0.0)
}

/// Overlap: count how many of the fact's set appear in `wanted`,
/// multiplied by `points_per`. Non-set facts score zero.
fn apply_overlap(wanted: &[String], points_per: f64, value: &FactValue) -> f64 {
    let FactValue::Set(items) = value else {
        return 0.0;
    };
    let count = items.iter().filter(|item| wanted.contains(item)).count();
    count as f64 * points_per
}

/// When-true: the score when the boolean is true, zero otherwise.
fn apply_when_true(points: f64, value: &FactValue) -> f64 {
    match value {
        FactValue::Boolean(true) => points,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_score_first_matching_upper_bound_and_unbounded_fallback() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 3,
            "name": "test",
            "parameters": [{
                "name": "stars",
                "weight": 3.0,
                "bands": [
                    { "max": 100, "points": 3.0 },
                    { "max": 1000, "points": 1.0 },
                    { "points": 0.0 }
                ]
            }]
        }))
        .unwrap();
        let report = score(
            &spec,
            &FitFacts {
                stars: 150,
                ..FitFacts::default()
            },
        );
        assert_eq!(report.total, 1.0);
        assert_eq!(report.maximum, 3.0);
        assert_eq!(report.parameters[0].points, 1.0);
    }

    #[test]
    fn invalid_parameter_is_reported_against_the_fixed_catalogue() {
        let result = parse_spec(
            r#"{"version":3,"name":"bad","parameters":[{"name":"shell","weight":1,"when_true":1}]}"#,
        );
        assert!(matches!(result, Err(FitError::UnknownParameter(name)) if name == "shell"));
    }

    #[test]
    fn default_spec_scores_deterministically_and_is_bounded() {
        let facts = FitFacts::default();
        let first = score(&default_spec(), &facts);
        let second = score(&default_spec(), &facts);
        assert_eq!(first, second);
        assert!(first.total >= 0.0 && first.total <= first.maximum);
        assert_eq!(PARAMETERS.len(), 18);
    }

    #[test]
    fn match_rule_scores_the_named_category() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "m",
            "parameters": [{
                "name": "license_class",
                "weight": 2.0,
                "match": { "permissive": 2.0, "copyleft": 0.0, "unknown": 0.5 }
            }]
        }))
        .unwrap();
        let facts = FitFacts {
            license_class: Some("permissive".to_string()),
            ..FitFacts::default()
        };
        assert_eq!(score(&spec, &facts).total, 2.0);
    }

    #[test]
    fn overlap_rule_counts_wanted_topics() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "o",
            "parameters": [{
                "name": "topics",
                "weight": 1.0,
                "overlap": 0.5,
                "wanted": ["wasm", "parser"]
            }]
        }))
        .unwrap();
        let facts = FitFacts {
            topics: vec!["wasm".to_string(), "other".to_string()],
            ..FitFacts::default()
        };
        assert_eq!(score(&spec, &facts).total, 0.5);
    }

    #[test]
    fn when_true_rule_scores_only_when_boolean_is_true() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "w",
            "parameters": [{
                "name": "local",
                "weight": 0.5,
                "when_true": 0.5
            }]
        }))
        .unwrap();
        let yes = FitFacts {
            local: true,
            ..FitFacts::default()
        };
        let no = FitFacts::default();
        assert_eq!(score(&spec, &yes).total, 0.5);
        assert_eq!(score(&spec, &no).total, 0.0);
    }

    #[test]
    fn every_catalogue_parameter_has_a_kind() {
        assert_eq!(parameter_kind("recency_days"), Some(ParamKind::Number));
        assert_eq!(parameter_kind("topics"), Some(ParamKind::Set));
        assert_eq!(parameter_kind("license_class"), Some(ParamKind::Category));
        assert_eq!(parameter_kind("local"), Some(ParamKind::Boolean));
        assert_eq!(parameter_kind("shell"), None);
        for name in PARAMETERS {
            assert!(parameter_kind(name).is_some(), "{name} has a kind");
        }
    }

    #[test]
    fn a_rule_that_disagrees_with_the_parameters_kind_is_refused() {
        let bands_on_a_set = parse_spec(
            r#"{"version":1,"name":"bad","parameters":[{"name":"topics","weight":1,"bands":[{"points":1}]}]}"#,
        );
        assert!(matches!(bands_on_a_set, Err(FitError::RuleMismatch(_))));
        let match_on_a_boolean = parse_spec(
            r#"{"version":1,"name":"bad","parameters":[{"name":"local","weight":1,"match":{"permissive":1}}]}"#,
        );
        assert!(matches!(match_on_a_boolean, Err(FitError::RuleMismatch(_))));
    }

    #[test]
    fn a_negative_weight_is_refused() {
        let result = parse_spec(
            r#"{"version":1,"name":"bad","parameters":[{"name":"local","weight":-1,"when_true":1}]}"#,
        );
        assert!(matches!(result, Err(FitError::BadValue(_))));
    }

    #[test]
    fn missing_fact_scores_zero_for_any_rule() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "x",
            "parameters": [
                { "name": "recency_days", "weight": 1.0, "bands": [{ "max": 10.0, "points": 1.0 }] },
                { "name": "license_class", "weight": 1.0, "match": { "permissive": 1.0 } },
                { "name": "local", "weight": 1.0, "when_true": 1.0 }
            ]
        }))
        .unwrap();
        assert_eq!(score(&spec, &FitFacts::default()).total, 0.0);
    }

    #[test]
    fn a_breakdown_written_before_versions_reads_back_with_version_zero() {
        let old = r#"{"total":3.5,"maximum":6.5,"parameters":[
            {"name":"local","weight":0.5,"points":0.5}]}"#;
        let report: FitReport = serde_json::from_str(old).expect("the old shape parses");
        assert_eq!(
            report.version, 0,
            "a breakdown from before versions reads as 0"
        );
        assert_eq!(report.total, 3.5);
        assert_eq!(report.parameters[0].name, "local");
    }

    #[test]
    fn a_rule_offering_more_than_its_weight_is_capped_at_the_weight() {
        let spec: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "two wanted topics, one point of weight",
            "parameters": [{
                "name": "topics",
                "weight": 1.0,
                "overlap": 1.0,
                "wanted": ["wasm", "parser"]
            }]
        }))
        .unwrap();
        let both = FitFacts {
            topics: vec!["wasm".to_string(), "parser".to_string()],
            ..FitFacts::default()
        };
        let report = score(&spec, &both);
        assert_eq!(report.parameters[0].points, 1.0, "capped at the weight");
        assert_eq!(report.total, report.maximum, "the total meets the maximum");
    }

    // --- The invariants: determinism, the weight ceiling, and the
    // --- pinned health boundaries no spec can move.

    fn bands_rule(bands: &[(Option<f64>, f64)]) -> FitRule {
        FitRule::Bands {
            bands: bands
                .iter()
                .map(|(max, points)| Band {
                    max: *max,
                    points: *points,
                })
                .collect(),
        }
    }

    fn parameter(name: &str, weight: f64, rule: FitRule) -> FitParameter {
        FitParameter {
            name: name.to_string(),
            weight,
            rule,
        }
    }

    fn every_parameter_spec() -> FitSpec {
        let rules: [(&str, f64, FitRule); 18] = [
            (
                "recency_days",
                3.0,
                bands_rule(&[(Some(90.0), 3.0), (Some(365.0), 1.0), (None, 0.0)]),
            ),
            (
                "created_days",
                1.0,
                bands_rule(&[(Some(365.0), 1.0), (None, 0.0)]),
            ),
            (
                "stars",
                2.0,
                bands_rule(&[(Some(100.0), 1.0), (Some(1000.0), 2.0), (None, 0.5)]),
            ),
            (
                "commit_count",
                1.0,
                bands_rule(&[(Some(1000.0), 1.0), (None, 0.0)]),
            ),
            (
                "commits_per_year",
                1.0,
                bands_rule(&[(Some(10.0), 1.0), (None, 0.0)]),
            ),
            (
                "release_age_days",
                1.0,
                bands_rule(&[(Some(30.0), 1.0), (None, 0.0)]),
            ),
            (
                "release_major",
                1.0,
                bands_rule(&[(Some(1.0), 1.0), (None, 0.0)]),
            ),
            (
                "license_class",
                2.0,
                FitRule::MatchRule {
                    values: BTreeMap::from([
                        ("permissive".to_string(), 2.0),
                        ("copyleft".to_string(), 0.0),
                        ("unknown".to_string(), 0.5),
                    ]),
                },
            ),
            (
                "language",
                1.0,
                FitRule::MatchRule {
                    values: BTreeMap::from([("Rust".to_string(), 1.0), ("C".to_string(), 0.5)]),
                },
            ),
            (
                "topics",
                1.0,
                FitRule::OverlapRule {
                    overlap: 1.0,
                    wanted: vec!["wasm".to_string(), "parser".to_string()],
                },
            ),
            (
                "health",
                2.0,
                bands_rule(&[(Some(4.0), 0.0), (Some(8.0), 1.0), (None, 2.0)]),
            ),
            (
                "readme_present",
                0.5,
                FitRule::WhenTrueRule { when_true: 0.5 },
            ),
            (
                "digest_present",
                0.5,
                FitRule::WhenTrueRule { when_true: 0.5 },
            ),
            ("is_fork", 0.5, FitRule::WhenTrueRule { when_true: 0.5 }),
            ("is_archived", 0.5, FitRule::WhenTrueRule { when_true: 0.5 }),
            ("local", 0.5, FitRule::WhenTrueRule { when_true: 0.5 }),
            ("tagged", 1.0, FitRule::WhenTrueRule { when_true: 1.0 }),
            (
                "starred_days",
                1.0,
                bands_rule(&[(Some(30.0), 1.0), (None, 0.0)]),
            ),
        ];
        FitSpec {
            version: 7,
            name: "every parameter".to_string(),
            parameters: rules
                .iter()
                .map(|(name, weight, rule)| parameter(name, *weight, rule.clone()))
                .collect(),
        }
    }

    /// The invariants' specs: the shipped default, the shape's own
    /// example, every catalogue parameter at once, a zero weight, and
    /// bands whose first tier is unbounded.
    fn spec_battery() -> Vec<FitSpec> {
        let example: FitSpec = serde_json::from_value(serde_json::json!({
            "version": 3,
            "name": "fit for a rust project",
            "parameters": [
                { "name": "recency_days", "weight": 3.0,
                  "bands": [ { "max": 90, "points": 3.0 },
                             { "max": 365, "points": 1.0 },
                             { "points": 0.0 } ] },
                { "name": "license_class", "weight": 2.0,
                  "match": { "permissive": 2.0, "copyleft": 0.0, "unknown": 0.5 } },
                { "name": "topics", "weight": 1.0, "overlap": 1.0,
                  "wanted": ["wasm", "parser"] },
                { "name": "local", "weight": 0.5, "when_true": 0.5 }
            ]
        }))
        .expect("the example spec parses");
        let zero = FitSpec {
            version: 1,
            name: "nothing counts".to_string(),
            parameters: vec![parameter(
                "local",
                0.0,
                FitRule::WhenTrueRule { when_true: 1.0 },
            )],
        };
        let unbounded_first = FitSpec {
            version: 2,
            name: "unbounded first".to_string(),
            parameters: vec![parameter(
                "stars",
                4.0,
                bands_rule(&[(None, 4.0), (Some(100.0), 1.0)]),
            )],
        };
        vec![
            default_spec(),
            example,
            every_parameter_spec(),
            zero,
            unbounded_first,
        ]
    }

    /// The invariants' facts: nothing collected, a little, and nearly
    /// everything.
    fn facts_battery() -> Vec<FitFacts> {
        let thin = FitFacts {
            stars: 3,
            health: 2,
            language: Some("C".to_string()),
            topics: vec!["tui".to_string()],
            ..FitFacts::default()
        };
        let rich = FitFacts {
            recency_days: Some(45.0),
            created_days: Some(800.0),
            stars: 640,
            commit_count: 4200,
            commits_per_year: Some(88.0),
            release_age_days: Some(12.0),
            release_major: Some(2),
            license_class: Some("permissive".to_string()),
            language: Some("Rust".to_string()),
            topics: vec!["wasm".to_string(), "parser".to_string()],
            health: 10,
            readme_present: true,
            digest_present: true,
            local: true,
            tagged: true,
            starred_days: Some(120.0),
            ..FitFacts::default()
        };
        vec![FitFacts::default(), thin, rich]
    }

    /// Every battery spec against every facts shape — one flat list, so
    /// the invariants stay single-looped.
    fn spec_fact_pairs() -> Vec<(FitSpec, FitFacts)> {
        let facts = facts_battery();
        spec_battery()
            .into_iter()
            .flat_map(|spec| facts.iter().cloned().map(move |fact| (spec.clone(), fact)))
            .collect()
    }

    #[test]
    fn invariant_any_spec_scores_deterministically() {
        for (spec, facts) in spec_fact_pairs() {
            assert_eq!(
                score(&spec, &facts),
                score(&spec, &facts),
                "spec `{}` scores the same twice",
                spec.name
            );
        }
    }

    #[test]
    fn invariant_no_report_exceeds_its_spec_maximum() {
        for (spec, facts) in spec_fact_pairs() {
            let report = score(&spec, &facts);
            assert!(
                report.total >= 0.0,
                "spec `{}` scores non-negative",
                spec.name
            );
            assert!(
                report.total <= report.maximum + 1e-9,
                "spec `{}` scored {} above its maximum {}",
                spec.name,
                report.total,
                report.maximum
            );
            for param in &report.parameters {
                assert!(
                    param.points <= param.weight + 1e-9,
                    "spec `{}` awarded `{}` {} above its weight {}",
                    spec.name,
                    param.name,
                    param.points,
                    param.weight
                );
            }
        }
    }

    fn edge_snapshot(
        created: chrono::DateTime<chrono::Utc>,
        stars: u64,
        last_commit: chrono::DateTime<chrono::Utc>,
    ) -> crate::model::RepoSnapshot {
        use crate::model::RepoSnapshot;
        RepoSnapshot {
            forge: crate::model::Forge::GitHub,
            repo_id: 1,
            owner: "owner".to_string(),
            name: "repo".to_string(),
            description: None,
            stars,
            languages: vec![],
            license: Some(crate::model::LicenseInfo {
                name: "MIT".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: vec![],
            default_branch: None,
            created_at: created,
            last_commit_at: last_commit,
            starred_at: None,
            commit_count: 10,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    #[test]
    fn invariant_no_spec_moves_the_pinned_health_boundaries() {
        use crate::health::health;
        use chrono::TimeZone;

        let scoring_now = chrono::Utc
            .with_ymd_and_hms(2026, 6, 15, 12, 0, 0)
            .single()
            .expect("valid test date");
        let pinned = [
            edge_snapshot(
                scoring_now - chrono::Duration::hours(24 * 365),
                100,
                scoring_now - chrono::Duration::days(1),
            ),
            edge_snapshot(
                scoring_now - chrono::Duration::hours(24 * 365 * 2),
                1000,
                scoring_now - chrono::Duration::days(365),
            ),
        ];
        let before: Vec<_> = pinned
            .iter()
            .map(|repo| health(repo, scoring_now))
            .collect();
        for (spec, facts) in spec_fact_pairs() {
            score(&spec, &facts);
        }
        for (repo, was) in pinned.iter().zip(&before) {
            assert_eq!(
                &health(repo, scoring_now),
                was,
                "scoring specs leaves the health boundaries where they were"
            );
        }
    }
}
