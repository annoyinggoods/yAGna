//! The forge's gate: a scan over the crate's sources that fails when the
//! UI toolkit or the database appears as text. The forge keeps the network
//! and nothing else.
//!
//! The gate lives outside `src` so that reading the sources it polices
//! does not itself violate the ban it enforces.

use std::fs;
use std::path::{Path, PathBuf};

/// The text tokens that must never appear in the crate's sources,
/// compared lowercased.
const BANNED: [&str; 2] = ["gpui", "rusqlite"];

#[test]
fn sources_stay_free_of_ui_and_database_text() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_files(&src, &mut files);
    assert!(!files.is_empty(), "the scan must see the crate's sources");
    for path in files {
        let text = fs::read_to_string(&path).expect("readable source");
        if let Some(token) = find_banned(&text) {
            panic!("{path:?} contains the banned token {token:?}");
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
