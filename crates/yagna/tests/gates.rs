//! The app crate's gates. One: no database or network text in this crate's
//! sources — the app only ever calls the facades. Two: no Rust source in
//! any crate of the workspace may reference the documentation tree — the
//! docs serve the humans building, never the binary built.
//!
//! The gates live outside `src` so that reading the sources they police
//! does not itself violate what they enforce.

use std::fs;
use std::path::{Path, PathBuf};

/// The text tokens that must never appear in this crate's sources: the app
/// opens no database and dials no API itself.
const BANNED: [&str; 2] = ["rusqlite", "octocrab"];

/// The documentation-tree marker, only ever flagged when a path separator
/// follows it.
const DOCS_MARKER: &str = "review";

#[test]
fn sources_stay_free_of_database_and_network_text() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    assert!(!files.is_empty(), "the scan must see the crate's sources");
    for path in files {
        let text = fs::read_to_string(&path).expect("readable source");
        if let Some(token) = find_banned(&text) {
            panic!("{path:?} contains the banned token {token:?}");
        }
    }
}

#[test]
fn no_crate_references_the_documentation_tree() {
    for path in all_crate_sources() {
        let text = fs::read_to_string(&path).expect("readable source");
        assert!(
            !references_docs(&text),
            "{path:?} references the documentation tree"
        );
    }
}

/// Every Rust source file in the workspace's crates — src trees, tests and
/// build scripts alike.
fn all_crate_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root")
        .join("crates");
    let mut files = Vec::new();
    collect_rust_files(&crates, &mut files);
    assert!(!files.is_empty(), "the scan must see the crates' sources");
    files
}

/// Collects every `.rs` file under `dir`, recursively.
fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

/// The first banned token present in `text`, if any.
fn find_banned(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    BANNED.into_iter().find(|token| lower.contains(token))
}

/// True when the text mentions the documentation tree with a path
/// separator — a reference into it, whatever the syntax around it.
fn references_docs(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut rest = lower.as_str();
    while let Some(index) = rest.find(DOCS_MARKER) {
        let after = &rest[index + DOCS_MARKER.len()..];
        if after.starts_with('/') || after.starts_with('\\') {
            return true;
        }
        rest = after;
    }
    false
}

/// The one Rust file allowed to name a color value: the applier.
const APPLIER: &str = "theme.rs";

/// The color-discipline gate: no hex color literal and no `rgb(` call
/// outside the applier. The theme files are data, not Rust, and the
/// scan sees Rust sources only.
#[test]
fn invariant_colors_flow_through_the_theme_applier() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    assert!(!files.is_empty(), "the scan must see the crate's sources");
    for path in files {
        if path.file_name().is_some_and(|name| name == APPLIER) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("readable source");
        if let Some(found) = find_color_literal(&text) {
            panic!("{path:?} names a color value ({found}); colors flow through ui::theme");
        }
    }
}

/// The first color literal in `text` — a `#rrggbb` hex string, an
/// `rgb(`/`rgba(` call, or a `0xRRGGBB` integer — if any.
/// Case-insensitive.
fn find_color_literal(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let bytes = lower.as_bytes();
    let hex_run = |at: usize| {
        bytes[at..]
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count()
    };
    // The walk steps char boundaries only — a slice from a middle of
    // a multibyte character would panic.
    for (index, _) in lower.char_indices() {
        if bytes[index] == b'#' && hex_run(index + 1) >= 6 {
            return Some(lower[index..index + 7].to_string());
        }
        for call in ["rgb(", "rgba("] {
            if lower[index..].starts_with(call) {
                return Some(call.to_string());
            }
        }
        if bytes[index] == b'0' && bytes.get(index + 1) == Some(&b'x') {
            let run = hex_run(index + 2);
            if (6..=8).contains(&run) {
                return Some(lower[index..index + 2 + run].to_string());
            }
        }
    }
    None
}
