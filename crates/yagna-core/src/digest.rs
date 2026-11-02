//! The gitingest-format digest, assembled from a repository's files:
//! the summary block, the directory-structure tree, and the `FILE:`
//! blocks between the `====` separators. The format is the one the
//! reader already knows from gitingest's output; the fixtures under
//! `tests/digest_fixtures/` are this format's source of truth.
//!
//! The token estimate is a heuristic — characters divided by four —
//! and is an estimate, never a promise. Binary files count and appear
//! in the tree but carry no body block. The block separator is a
//! 48-column line of `=`.
//!
//! Items: `DigestFile` — one file of the digest (path, size, optional
//! text); `Caps` — the cap that stopped a capped walk; `DigestInput` —
//! the formatter's input; `filter_files` — the spec's include/exclude
//! globs and per-file cap applied; `format_digest` — input to text;
//! `standard_spec` — the default spec; `text_of` — bytes to optional
//! text (the binary sniff); `estimate_tokens`/`token_estimate` — the
//! heuristic; `SEPARATOR` — the block separator.
//! Depends on: globset (pure glob matching), crate::model (the spec and
//! the refs), crate::treefmt (the tree render).
//! Called by: yagna-forge (the tarball walk classifies files through
//! `text_of`), yagna-store (the store formats and keeps the body), the
//! app (the digest section renders the stored body).

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::model::{DigestSpec, RepoRef, TreeEntry, TreeEntryKind};
use crate::treefmt;

/// The line that opens and closes every `FILE:` block — gitingest's own
/// width, the one the reader recognises.
pub const SEPARATOR: &str = "================================================";

/// The per-file size cap of the standard spec: a file larger than this
/// is dropped from the digest entirely.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 256 * 1024;

/// The mid-stream total-bytes cap: a walk past this stops, and the
/// digest says so in its summary.
pub const DEFAULT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// The mid-stream file-count cap: a walk past this stops, and the
/// digest says so in its summary.
pub const DEFAULT_MAX_FILES: usize = 20_000;

/// How many leading bytes the binary sniff looks at for a NUL.
const BINARY_SNIFF_BYTES: usize = 8_192;

/// The exclude patterns the standard spec carries: lockfiles, vendored
/// trees, the git internals. Globset's `**` forms catch the nested
/// copies (a vendored `.git` inside a submodule checkout) as well as
/// the root ones.
pub const DEFAULT_EXCLUDES: [&str; 11] = [
    "*.lock",
    "Cargo.lock",
    "package-lock.json",
    "node_modules/**",
    "**/node_modules/**",
    "target/**",
    "**/target/**",
    "vendor/**",
    "**/vendor/**",
    ".git/**",
    "**/.git/**",
];

/// One file of a digest: its path from the repository root, its size in
/// bytes, and its text — absent for binaries (which count and appear in
/// the tree but have no body block) and for files whose content was
/// skipped at the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestFile {
    pub path: String,
    pub bytes: u64,
    #[serde(default)]
    pub text: Option<String>,
}

/// The cap a walk ran into. The digest names it and its limit in its
/// own summary, so a truncated digest is never mistaken for a whole one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caps {
    /// The walk stopped because the files' content reached this many
    /// bytes.
    TotalBytes { limit: u64 },
    /// The walk stopped because it had listed this many files.
    FileCount { limit: usize },
}

impl Caps {
    /// The summary line's reason, in words.
    pub fn describe(&self) -> String {
        match self {
            Caps::TotalBytes { limit } => {
                format!("the total size cap of {limit} bytes stopped the walk")
            }
            Caps::FileCount { limit } => {
                format!("the file count cap of {limit} files stopped the walk")
            }
        }
    }
}

/// The formatter's input: one repository, its files, and the cap that
/// stopped the walk — none when the walk ran to the end. `taken_commit`
/// is provenance for the store alone (the branch head the tarball was
/// cut from); the formatter itself never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestInput {
    pub repo: RepoRef,
    pub files: Vec<DigestFile>,
    #[serde(default)]
    pub truncated_by: Option<Caps>,
    #[serde(default)]
    pub taken_commit: Option<String>,
}

/// The standard spec: the repository, no include filter, the default
/// excludes, and the default per-file cap. The cap is what takes media
/// and other big binaries out of the digest — anything larger than it
/// contributes nothing but noise.
pub fn standard_spec(repo: &RepoRef) -> DigestSpec {
    DigestSpec {
        repo: repo.clone(),
        include: Vec::new(),
        exclude: DEFAULT_EXCLUDES
            .iter()
            .map(|glob| glob.to_string())
            .collect(),
        max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
    }
}

/// Applies the spec to the files: the include globs must match (when
/// the spec carries any), the exclude globs must not, and a file larger
/// than the spec's per-file cap is dropped. The order of the survivors
/// is the order of the input.
pub fn filter_files(files: Vec<DigestFile>, spec: &DigestSpec) -> Vec<DigestFile> {
    let include = globset_of(&spec.include);
    let exclude = globset_of(&spec.exclude);
    files
        .into_iter()
        .filter(|file| keep_file(file, &include, &exclude, spec.max_file_bytes))
        .collect()
}

/// Whether one file survives the spec.
fn keep_file(file: &DigestFile, include: &GlobSet, exclude: &GlobSet, cap: Option<u64>) -> bool {
    if !include.is_empty() && !include.is_match(&file.path) {
        return false;
    }
    if exclude.is_match(&file.path) {
        return false;
    }
    match cap {
        Some(limit) => file.bytes <= limit,
        None => true,
    }
}

/// The patterns compiled into a set. A pattern that does not compile is
/// skipped — the spec's globs are a convenience, not a query language,
/// and one bad pattern must not lose the whole digest. `*` does not
/// cross a `/`: the vendored-tree patterns name whole directories.
fn globset_of(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(true).build() {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// The digest text: the summary block, the directory-structure tree,
/// then one `FILE:` block per file that has a body. Files without a
/// body (binaries, skipped content) appear only in the tree and the
/// count. The tree renders through the core's own tree format.
pub fn format_digest(input: &DigestInput) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Repository: {}/{}\n",
        input.repo.owner, input.repo.name
    ));
    out.push_str(&format!("Files analyzed: {}\n", input.files.len()));
    if let Some(caps) = input.truncated_by.as_ref() {
        out.push_str(&format!("Truncated: {}\n", caps.describe()));
    }
    out.push_str(&format!(
        "\nEstimated tokens: {}\n\n",
        format_tokens(token_estimate(input))
    ));
    out.push_str("Directory structure:\n");
    out.push_str(&tree_text(&input.files));
    out.push('\n');
    for file in &input.files {
        let Some(text) = file.text.as_ref() else {
            continue;
        };
        out.push_str(SEPARATOR);
        out.push('\n');
        out.push_str("FILE: ");
        out.push_str(&file.path);
        out.push('\n');
        out.push_str(SEPARATOR);
        out.push('\n');
        out.push_str(text.trim_end_matches('\n'));
        out.push_str("\n\n");
    }
    out
}

/// The structure tree of the digest's files, rendered through the core's
/// tree format. An empty file list renders a stated nothing — a digest
/// of zero files is still a digest.
fn tree_text(files: &[DigestFile]) -> String {
    let entries: Vec<TreeEntry> = files
        .iter()
        .map(|file| TreeEntry {
            path: file.path.clone(),
            kind: TreeEntryKind::Blob,
            size: Some(file.bytes),
        })
        .collect();
    let rendered = treefmt::render(&entries, false);
    if rendered.is_empty() {
        "(no files)\n".to_string()
    } else {
        rendered
    }
}

/// The token estimate for one input: the tree plus every body, by the
/// characters-divided-by-four heuristic. The summary block itself is
/// not counted.
pub fn token_estimate(input: &DigestInput) -> u64 {
    let tree = tree_text(&input.files);
    let body_chars: usize = input
        .files
        .iter()
        .filter_map(|file| file.text.as_ref())
        .map(|text| text.chars().count())
        .sum();
    estimate_chars(tree.chars().count() + body_chars)
}

/// The heuristic on a character count: characters divided by four. An
/// estimate of tokens, not a count of them.
fn estimate_chars(chars: usize) -> u64 {
    (chars / 4) as u64
}

/// The heuristic on a text: `estimate_tokens` is what the store and the
/// tests reach for when they need the number without an input.
pub fn estimate_tokens(text: &str) -> u64 {
    estimate_chars(text.chars().count())
}

/// The summary's token figure: gitingest's shape — `15.2k`, `1.2M`,
/// plain under a thousand — with the `~` that says estimate.
pub fn format_tokens(tokens: u64) -> String {
    let figure = if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    };
    format!("~{figure}")
}

/// File bytes as optional text: a NUL in the sniff window makes it
/// binary (no text), and bytes that are not UTF-8 make it binary too.
/// Everything else is the file's text.
pub fn text_of(bytes: &[u8]) -> Option<String> {
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Forge;

    fn repo() -> RepoRef {
        RepoRef {
            forge: Forge::GitHub,
            owner: "someone".to_string(),
            name: "something".to_string(),
        }
    }

    fn file(path: &str, bytes: u64, text: Option<&str>) -> DigestFile {
        DigestFile {
            path: path.to_string(),
            bytes,
            text: text.map(|text| text.to_string()),
        }
    }

    #[test]
    fn the_summary_carries_the_identity_the_count_and_the_estimate() {
        let input = DigestInput {
            repo: repo(),
            files: vec![file("README.md", 40, Some("# Demo\n"))],
            truncated_by: None,
            taken_commit: None,
        };
        let body = format_digest(&input);
        assert!(body.starts_with("Repository: someone/something\n"));
        assert!(body.contains("Files analyzed: 1\n"));
        assert!(
            body.contains("Estimated tokens: "),
            "the estimate line: {body}"
        );
        assert!(
            body.contains("Directory structure:\n└── README.md\n"),
            "the tree renders: {body}"
        );
    }

    #[test]
    fn binary_files_are_listed_but_bodyless() {
        let input = DigestInput {
            repo: repo(),
            files: vec![
                file("logo.png", 4_096, None),
                file("main.rs", 12, Some("fn main() {}\n")),
            ],
            truncated_by: None,
            taken_commit: None,
        };
        let body = format_digest(&input);
        assert_eq!(body.matches("FILE: ").count(), 1, "only the text file");
        assert!(body.contains("Files analyzed: 2\n"));
        assert!(body.contains("├── logo.png\n"), "the binary is in the tree");
    }

    #[test]
    fn a_capped_digest_says_so_in_its_summary() {
        let input = DigestInput {
            repo: repo(),
            files: vec![file("a.rs", 10, None)],
            truncated_by: Some(Caps::FileCount { limit: 1 }),
            taken_commit: None,
        };
        let body = format_digest(&input);
        assert!(
            body.contains("Truncated: the file count cap of 1 files stopped the walk\n"),
            "the cap is named: {body}"
        );
        let other = DigestInput {
            truncated_by: Some(Caps::TotalBytes { limit: 1024 }),
            ..input
        };
        assert!(
            format_digest(&other).contains("the total size cap of 1024 bytes stopped the walk"),
            "the byte cap is named"
        );
    }

    #[test]
    fn the_filter_applies_includes_excludes_and_the_cap() {
        let spec = DigestSpec {
            repo: repo(),
            include: vec!["src/**".to_string()],
            exclude: vec!["src/generated/**".to_string()],
            max_file_bytes: Some(100),
        };
        let files = vec![
            file("README.md", 10, Some("# x")),
            file("src/main.rs", 10, Some("fn main() {}")),
            file("src/generated/out.rs", 10, Some("generated")),
            file("src/huge.rs", 1_000, Some("huge")),
        ];
        let kept = filter_files(files, &spec);
        let paths: Vec<&str> = kept.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/main.rs"],
            "include wins, exclude second, cap third"
        );
    }

    #[test]
    fn the_standard_spec_excludes_lockfiles_vendored_trees_and_git() {
        let spec = standard_spec(&repo());
        let files = vec![
            file("Cargo.lock", 10, Some("lock")),
            file("package-lock.json", 10, Some("lock")),
            file("app.lock", 10, Some("lock")),
            file("node_modules/left-pad/index.js", 10, Some("x")),
            file("target/debug/main.rs", 10, Some("x")),
            file("vendor/lib/lib.rs", 10, Some("x")),
            file(".git/config", 10, Some("x")),
            file("src/main.rs", 10, Some("fn main() {}")),
            file("logo.png", 4_096, None),
        ];
        let kept = filter_files(files, &spec);
        let paths: Vec<&str> = kept.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["src/main.rs", "logo.png"]);
        assert_eq!(spec.max_file_bytes, Some(DEFAULT_MAX_FILE_BYTES));
    }

    #[test]
    fn an_uncompilable_glob_is_skipped_not_fatal() {
        let spec = DigestSpec {
            repo: repo(),
            include: Vec::new(),
            exclude: vec!["[[".to_string(), "src/**".to_string()],
            max_file_bytes: None,
        };
        let files = vec![
            file("src/main.rs", 1, Some("x")),
            file("lib.rs", 1, Some("x")),
        ];
        let kept = filter_files(files, &spec);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, "lib.rs");
    }

    #[test]
    fn text_of_sniffs_binaries_and_decodes_utf8() {
        assert_eq!(text_of(b"# plain text"), Some("# plain text".to_string()));
        assert_eq!(text_of(b"caf\xC3\xA9"), Some("café".to_string()));
        assert_eq!(text_of(b"png\x00data"), None, "a NUL makes it binary");
        assert_eq!(
            text_of(&[0xFF, 0xFE]),
            None,
            "invalid UTF-8 makes it binary"
        );
    }

    #[test]
    fn the_token_figures_render_gitingest_style() {
        assert_eq!(format_tokens(29), "~29");
        assert_eq!(format_tokens(15_200), "~15.2k");
        assert_eq!(format_tokens(1_234_567), "~1.2M");
    }

    #[test]
    fn the_estimate_is_characters_divided_by_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("abcdef"), 1, "floor, not round");
    }

    #[test]
    fn an_empty_digest_still_carries_its_block_shape() {
        let input = DigestInput {
            repo: repo(),
            files: Vec::new(),
            truncated_by: None,
            taken_commit: None,
        };
        let body = format_digest(&input);
        assert!(body.contains("Files analyzed: 0\n"));
        assert!(
            body.contains("Directory structure:\n(no files)\n"),
            "{body}"
        );
    }
}
