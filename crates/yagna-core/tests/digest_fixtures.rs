//! The digest's golden fixtures: every `digest_fixtures/*.json` case is
//! filtered by its spec and formatted, and the result must equal the
//! paired `.expected.txt` byte for byte. The fixtures are the digest
//! format's source of truth — a change to the format lands together
//! with its regenerated fixtures.
//!
//! The `gitingest-shape` fixture mirrors the research digest's repo
//! shape; the separators and `FILE:` lines match gitingest's output.
//! Two facts diverge by decision, both pinned here: the structure tree
//! is the core's own treefmt render (no root-directory prefix), and the
//! token estimate is the `~`-prefixed chars-divided-by-four figure.
//!
//! `--ignored` helper: `golden_bodies` prints every case's digest body,
//! so a deliberate format change can be diffed and pasted into the
//! `.expected.txt` files.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use yagna_core::digest::{self, Caps, DigestFile, DigestInput};
use yagna_core::model::{DigestSpec, RepoRef};

/// One fixture case, as its JSON file holds it: the repository, the
/// spec choice (the standard one, or an inline custom spec), the files
/// the tarball yielded, and the cap that stopped the walk — none when
/// it ran to the end.
#[derive(Deserialize)]
struct FixtureCase {
    repo: RepoRef,
    spec: SpecChoice,
    #[serde(default)]
    taken_commit: Option<String>,
    #[serde(default)]
    truncated_by: Option<Caps>,
    files: Vec<DigestFile>,
}

/// The spec the case filters with: the standard one, or an inline one
/// (the shape the digests table keeps as its `spec_json`). The
/// standard choice names itself — any other word is a fixture typo,
/// not a spec.
#[derive(Deserialize)]
#[serde(untagged)]
enum SpecChoice {
    Standard(String),
    Custom(DigestSpec),
}

/// Every fixture stem: the `.json` and the `.expected.txt` that must
/// match its digest body.
fn fixture_stems() -> Vec<String> {
    let dir = fixtures_dir();
    let mut stems = Vec::new();
    for entry in fs::read_dir(&dir).expect("fixtures directory readable") {
        let path = entry.expect("fixture entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            stems.push(
                path.file_stem()
                    .expect("stemmed")
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    stems.sort();
    assert!(
        !stems.is_empty(),
        "the fixture directory must carry at least one case"
    );
    stems
}

/// The fixtures' directory, beside this test file.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("digest_fixtures")
}

/// The case's digest body: files filtered by the spec, then formatted.
fn digest_body(case: &FixtureCase) -> String {
    let spec = match &case.spec {
        SpecChoice::Standard(word) if word == "standard" => digest::standard_spec(&case.repo),
        SpecChoice::Standard(word) => {
            panic!("unknown spec choice {word:?} — write \"standard\" or an inline spec")
        }
        SpecChoice::Custom(spec) => spec.clone(),
    };
    let files = digest::filter_files(case.files.clone(), &spec);
    let input = DigestInput {
        repo: case.repo.clone(),
        files,
        truncated_by: case.truncated_by.clone(),
        taken_commit: case.taken_commit.clone(),
    };
    digest::format_digest(&input)
}

#[test]
fn every_digest_fixture_formats_to_its_expected_body() {
    for stem in fixture_stems() {
        let case: FixtureCase = serde_json::from_str(
            &fs::read_to_string(fixtures_dir().join(format!("{stem}.json")))
                .unwrap_or_else(|error| panic!("{stem}.json unreadable: {error}")),
        )
        .unwrap_or_else(|error| panic!("{stem}.json is not a fixture case: {error}"));
        let expected = fs::read_to_string(fixtures_dir().join(format!("{stem}.expected.txt")))
            .unwrap_or_else(|error| panic!("{stem}.expected.txt unreadable: {error}"));
        assert_eq!(digest_body(&case), expected, "fixture {stem}");
    }
}

/// The maintenance helper: prints every case's digest body between
/// markers, so a deliberate format change can be pasted into the
/// `.expected.txt` files. Run with
/// `cargo test --test digest_fixtures -- --ignored --nocapture`.
#[test]
#[ignore = "a maintenance helper: prints the golden bodies, never asserts"]
fn golden_bodies() {
    for stem in fixture_stems() {
        let case: FixtureCase = serde_json::from_str(
            &fs::read_to_string(fixtures_dir().join(format!("{stem}.json"))).expect("readable"),
        )
        .expect("a fixture case");
        println!("===== {stem}.expected.txt =====");
        println!("{}", digest_body(&case));
        println!("===== end {stem} =====");
    }
}
