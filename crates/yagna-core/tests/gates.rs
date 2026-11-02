//! The core-freedom gate: a scan over the crate's sources that fails when
//! platform conditionals, files, clocks, the network, the UI toolkit or
//! the database appear as text. The core stays pure.
//!
//! The gate lives outside `src` so that reading the sources it polices
//! does not itself violate the ban it enforces.

use std::fs;
use std::path::{Path, PathBuf};

/// The text tokens that must never appear in the crate's sources,
/// compared lowercased.
const BANNED: [&str; 8] = [
    "std::fs",
    "tokio",
    "reqwest",
    "octocrab",
    "gpui",
    "rusqlite",
    "utc::now",
    "instant::now",
];

#[test]
fn sources_stay_pure_of_worldly_text() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_files(&src, &mut files);
    assert!(!files.is_empty(), "the scan must see the crate's sources");
    for path in files {
        let text = fs::read_to_string(&path).expect("readable source");
        if let Some(token) = find_banned(&text) {
            panic!("{path:?} contains the banned token {token:?}");
        }
        if contains_cfg_target(&text) {
            panic!("{path:?} contains a cfg(target…) conditional");
        }
    }
}

/// Collects every file under `dir`, recursively.
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// The first banned token present in `text`, if any.
fn find_banned(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    BANNED.into_iter().find(|token| lower.contains(token))
}

/// True when the text contains `cfg` followed by optional whitespace, an
/// opening parenthesis, more whitespace, and `target`.
fn contains_cfg_target(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut rest = lower.as_str();
    while let Some(index) = rest.find("cfg") {
        if cfg_target_after(&rest[index + 3..]) {
            return true;
        }
        rest = &rest[index + 3..];
    }
    false
}

/// The cfg-target check for the text right after a `cfg` occurrence.
fn cfg_target_after(after_cfg: &str) -> bool {
    let after = after_cfg.trim_start();
    match after.strip_prefix('(') {
        Some(inner) => inner.trim_start().starts_with("target"),
        None => false,
    }
}
