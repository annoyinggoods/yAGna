//! The fit score's spec file (`<data>/fit.json`): the read side on the
//! settings' contract and the write side with validation and backup.
//!
//! The read contract has three tiers: an absent file is the default
//! spec and silence; a broken file is the default spec plus one loud
//! line naming the file; a bad parameter value falls back to the
//! default spec's same-name parameter (or, when the default has none,
//! is dropped) while the rest of the file stands. The write side
//! validates strictly — the same core parser the agent's rewrite is
//! refused by — and keeps the replaced file at `fit.previous.json`.
//!
//! Items: none — this module is functions only.
//! Core functions: `load` — the spec plus the loud line; `load_from` /
//! `save_to` — the read contract and the validated, backed-up write
//! against a chosen path; `changed_parameters` — what two specs
//! disagree on; `changed_at` — the file's last change; `spec_path` —
//! the file's location.
//! Depends on: yagna_core::fit (the spec and its validation),
//! crate::paths (the data directory).
//! Called by: crate::mcp (the tools) and crate::ui (the score surface).

use std::path::{Path, PathBuf};

use yagna_core::fit::{FitParameter, FitSpec, default_spec, parse_spec};

/// The spec's file name inside the data directory.
const SPEC_FILE: &str = "fit.json";

/// Where the replaced spec goes before the new one lands.
const BACKUP_FILE: &str = "fit.previous.json";

/// The spec the session runs with, plus the loud line when the file
/// said something the default had to cover for.
pub(crate) fn load() -> (FitSpec, Option<String>) {
    let Ok(dir) = crate::paths::data_dir() else {
        return (default_spec(), None);
    };
    load_from(&dir.join(SPEC_FILE))
}

/// The spec file's location inside the data directory.
pub(crate) fn spec_path() -> Result<PathBuf, String> {
    Ok(crate::paths::data_dir()
        .map_err(|error| format!("fit spec: {error}"))?
        .join(SPEC_FILE))
}

/// When the spec file last changed — the score surface's footer fact.
/// `None` when the file is not there (or its time cannot be read).
pub(crate) fn changed_at() -> Option<chrono::DateTime<chrono::Utc>> {
    let path = crate::paths::data_dir().ok()?.join(SPEC_FILE);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some(modified.into())
}

/// `load`, against a caller-chosen path — the test seam and the
/// server's injectable data directory.
pub(crate) fn load_from(path: &Path) -> (FitSpec, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (default_spec(), None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (default_spec(), Some(broken_line(path)));
    };
    spec_from_document(&value, path)
}

/// The per-parameter conversion: a document that is not the spec's
/// shape is broken outright; a parameter that carries a bad value
/// falls back to the default spec's same-name parameter alone.
fn spec_from_document(value: &serde_json::Value, path: &Path) -> (FitSpec, Option<String>) {
    let Some(parameters) = value.get("parameters").and_then(|p| p.as_array()) else {
        return (default_spec(), Some(broken_line(path)));
    };
    if document_scalars_broken(value) {
        return (default_spec(), Some(broken_line(path)));
    }
    let fallbacks = default_spec();
    let mut out = Vec::new();
    let mut touched = Vec::new();
    for entry in parameters {
        match parameter_from(entry, &fallbacks) {
            ParameterOutcome::Carried(param) => out.push(param),
            ParameterOutcome::FellBack(param, name) => {
                out.push(param);
                touched.push(name);
            }
            ParameterOutcome::Dropped(name) => touched.push(name),
        }
    }
    let spec = FitSpec {
        version: document_number(value, "version").unwrap_or(1),
        name: document_text(value, "name").unwrap_or_else(|| "fit spec".to_string()),
        parameters: out,
    };
    let line = loud_line(path, &touched);
    (spec, line)
}

/// One file parameter's fate: carried as written, replaced by the
/// default's same-name parameter, or dropped when the default has no
/// same-name parameter either.
enum ParameterOutcome {
    Carried(FitParameter),
    FellBack(FitParameter, String),
    Dropped(String),
}

fn parameter_from(entry: &serde_json::Value, fallbacks: &FitSpec) -> ParameterOutcome {
    let Ok(param) = serde_json::from_value::<FitParameter>(entry.clone()) else {
        return ParameterOutcome::Dropped(parameter_name(entry));
    };
    let name = param.name.clone();
    match parse_spec(&serialize_one(&param)) {
        Ok(_) => ParameterOutcome::Carried(param),
        Err(_) => match fallbacks.parameters.iter().find(|p| p.name == name) {
            Some(defaulted) => ParameterOutcome::FellBack(defaulted.clone(), name),
            None => ParameterOutcome::Dropped(name),
        },
    }
}

/// The name a malformed parameter object claims to carry, for the loud
/// line. An entry that names itself nothing is named `?`.
fn parameter_name(entry: &serde_json::Value) -> String {
    entry
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("?")
        .to_string()
}

/// One parameter as a whole spec document — the shape the core's
/// parser validates. One parameter, one catalogue check: the parser is
/// the single validation implementation.
fn serialize_one(param: &FitParameter) -> String {
    let spec = FitSpec {
        version: 1,
        name: "check".to_string(),
        parameters: vec![param.clone()],
    };
    serde_json::to_string(&spec).unwrap_or_default()
}

/// Whether the document's own scalars disagree with the spec's shape:
/// a version that is not a number, a name that is not text. Absent is
/// fine — the defaults fill in; wrong-typed is broken.
fn document_scalars_broken(value: &serde_json::Value) -> bool {
    let version_wrong = value
        .get("version")
        .is_some_and(|v| !v.is_u64() && !v.is_null());
    let name_wrong = value
        .get("name")
        .is_some_and(|v| !v.is_string() && !v.is_null());
    version_wrong || name_wrong
}

fn document_number(value: &serde_json::Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32)
}

fn document_text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// The one loud line for a broken file.
fn broken_line(path: &Path) -> String {
    format!(
        "fit spec file {}: not the fit spec's JSON shape — running on the default spec",
        path.display()
    )
}

/// The one loud line for the parameters that fell back or were
/// dropped.
fn loud_line(path: &Path, touched: &[String]) -> Option<String> {
    if touched.is_empty() {
        return None;
    }
    Some(format!(
        "fit spec file {}: parameter(s) {} fell back to the default — the rest of the file stands",
        path.display(),
        touched.join(", ")
    ))
}

/// `save`, against a caller-chosen path — the test seam and the
/// server's injectable data directory. The write validates the spec
/// first, then backs the standing file up and lands the new one
/// atomically.
pub(crate) fn save_to(path: &Path, spec: &FitSpec) -> Result<(), String> {
    let text = serde_json::to_string_pretty(spec)
        .map_err(|error| format!("fit spec cannot be written: {error}"))?;
    parse_spec(&text).map_err(|error| format!("fit spec refused: {error}"))?;
    if path.exists() {
        let backup = path.with_file_name(BACKUP_FILE);
        std::fs::copy(path, &backup)
            .map_err(|error| format!("fit spec backup {}: {error}", backup.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &text)
        .map_err(|error| format!("fit spec file {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|error| format!("fit spec file {}: {error}", path.display()))
}

/// The parameter names two specs disagree on: added, removed, or
/// changed — sorted, one name per touch. The surface's "what changed"
/// line and the agent's write report are both this.
pub(crate) fn changed_parameters(before: &FitSpec, after: &FitSpec) -> Vec<String> {
    let mut touched = Vec::new();
    for param in &before.parameters {
        match after.parameters.iter().find(|p| p.name == param.name) {
            Some(other) if other == param => {}
            Some(_) => touched.push(param.name.clone()),
            None => touched.push(param.name.clone()),
        }
    }
    for param in &after.parameters {
        if !before.parameters.iter().any(|p| p.name == param.name) {
            touched.push(param.name.clone());
        }
    }
    touched.sort();
    touched.dedup();
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("yagna-fitspec-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn an_absent_spec_file_is_the_default_spec_and_silence() {
        let dir = scratch("absent");
        let (spec, line) = load_from(&dir.join("fit.json"));
        assert_eq!(spec, default_spec());
        assert_eq!(line, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_spec_file_is_the_default_spec_and_a_line_naming_the_file() {
        let dir = scratch("broken");
        let path = dir.join("fit.json");
        std::fs::write(&path, "{ not json").expect("write");
        let (spec, line) = load_from(&path);
        assert_eq!(spec, default_spec());
        let line = line.expect("loud");
        assert!(line.contains("fit.json"), "the line names the file: {line}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_typed_document_scalar_is_broken_outright() {
        let dir = scratch("bad-scalar");
        let path = dir.join("fit.json");
        std::fs::write(&path, r#"{"version":"x","name":"mine","parameters":[]}"#).expect("write");
        let (spec, line) = load_from(&path);
        assert_eq!(spec, default_spec(), "the file is broken, not half-read");
        assert!(line.expect("loud").contains("fit.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_parameter_falls_back_to_the_default_for_that_parameter_alone() {
        let dir = scratch("bad-value");
        let path = dir.join("fit.json");
        std::fs::write(
            &path,
            r#"{"version":1,"name":"mine","parameters":[
                {"name":"recency_days","weight":3,"bands":[]},
                {"name":"local","weight":1,"when_true":0.5}]}"#,
        )
        .expect("write");
        let (spec, line) = load_from(&path);
        let defaults = default_spec();
        let recency = spec
            .parameters
            .iter()
            .find(|p| p.name == "recency_days")
            .expect("recency kept");
        let default_recency = defaults
            .parameters
            .iter()
            .find(|p| p.name == "recency_days")
            .expect("default recency");
        assert_eq!(
            recency, default_recency,
            "the bad one fell back to the default"
        );
        let local = spec
            .parameters
            .iter()
            .find(|p| p.name == "local")
            .expect("local kept");
        assert_eq!(
            local.rule,
            yagna_core::fit::FitRule::WhenTrueRule { when_true: 0.5 },
            "the good one stands"
        );
        let line = line.expect("loud");
        assert!(
            line.contains("recency_days"),
            "the line names the parameter: {line}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_parameter_falls_back_to_nothing_and_is_named() {
        let dir = scratch("unknown");
        let path = dir.join("fit.json");
        std::fs::write(
            &path,
            r#"{"version":1,"name":"mine","parameters":[
                {"name":"shell","weight":1,"when_true":1},
                {"name":"local","weight":1,"when_true":0.5}]}"#,
        )
        .expect("write");
        let (spec, line) = load_from(&path);
        assert!(
            !spec.parameters.iter().any(|p| p.name == "shell"),
            "the unknown parameter is dropped"
        );
        assert!(spec.parameters.iter().any(|p| p.name == "local"));
        let line = line.expect("loud");
        assert!(
            line.contains("shell"),
            "the line names the parameter: {line}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_writes_the_spec_and_backs_up_the_previous_file() {
        let dir = scratch("save");
        let path = dir.join("fit.json");
        let first = default_spec();
        save_to(&path, &first).expect("first save");
        let mut second = default_spec();
        second.parameters[0].weight = 9.0;
        save_to(&path, &second).expect("second save");
        let (current, line) = load_from(&path);
        assert_eq!(current, second);
        assert_eq!(line, None, "a saved spec reads back clean");
        let (previous, _) = load_from(&path.with_file_name(BACKUP_FILE));
        assert_eq!(previous, first, "the replaced spec is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_a_spec_outside_the_catalogue() {
        let dir = scratch("refuse");
        let path = dir.join("fit.json");
        let mut bad = default_spec();
        bad.parameters[0].name = "shell".to_string();
        let error = save_to(&path, &bad).expect_err("refused");
        assert!(
            error.contains("shell"),
            "the message names the parameter: {error}"
        );
        assert!(
            error.contains("catalogue"),
            "the message names the catalogue: {error}"
        );
        assert!(!path.exists(), "nothing was written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_parameters_counts_the_touches() {
        let before = default_spec();
        let mut changed = default_spec();
        changed.parameters[0].weight += 1.0;
        assert_eq!(changed_parameters(&before, &changed), ["recency_days"]);
        let mut removed = default_spec();
        removed.parameters.pop();
        let touches = changed_parameters(&before, &removed);
        assert_eq!(touches.len(), 1);
        assert_eq!(touches[0], "local");
        let mut added = default_spec();
        added.parameters.push(added.parameters[0].clone());
        added.parameters.last_mut().expect("pushed").name = "topics".to_string();
        let touches = changed_parameters(&before, &added);
        assert_eq!(touches, ["topics"]);
    }
}
