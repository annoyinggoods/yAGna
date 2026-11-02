//! The expanded row's preview over the fixture corpus: the README the
//! audit caught, read the way the row reads it.
//!
//! The pipeline is the real one: the fixture is rewritten (the image
//! destinations become asset ids, the badge flag rides along), the
//! renderer answers every id, and the preview collapses the badges into
//! a count. What the reader would see is the preview's visible text —
//! and it never carries the raw link syntax.
//!
//! The corpus: fixture file name and what the table asserts about it.
//! The fixtures are package data — embedded at compile time via
//! `include_str!`, never a repository read at run time.

use std::collections::HashMap;
use std::path::PathBuf;

use yagna_core::markdown::{AssetEntry, RawBase, rewrite_assets};
use yagna_core::mdrender::{AssetResolver, Resolved, count_badges, preview, visible_text};

/// The corpus: fixture file name and what the table asserts about it.
const CORPUS: [(&str, &str); 2] = [
    ("badge-heavy.md", "a badge wall, a table and a link"),
    ("plain.md", "prose and one link, no badge at all"),
];

fn fixture(name: &str) -> &'static str {
    match name {
        "badge-heavy.md" => include_str!("preview_fixtures/badge-heavy.md"),
        "plain.md" => include_str!("preview_fixtures/plain.md"),
        other => panic!("unknown fixture {other}"),
    }
}

fn base() -> RawBase {
    RawBase {
        owner: "example".to_string(),
        repo: "everything".to_string(),
        branch: "main".to_string(),
    }
}

/// The resolver the reader's surfaces share: the badge ids answer as a
/// badge, every other id answers as a cached file.
struct Fixed {
    answers: HashMap<String, Resolved>,
}

impl Fixed {
    fn new(entries: &[AssetEntry]) -> Self {
        let answers = entries
            .iter()
            .map(|entry| {
                let answer = if entry.badge {
                    Resolved::Badge
                } else {
                    Resolved::File(PathBuf::from(format!(
                        "/cache/{}.img",
                        entry.image.asset_id
                    )))
                };
                (entry.image.asset_id.clone(), answer)
            })
            .collect();
        Self { answers }
    }
}

impl AssetResolver for Fixed {
    fn resolve(&self, asset_id: &str) -> Resolved {
        self.answers
            .get(asset_id)
            .cloned()
            .unwrap_or(Resolved::Missing)
    }
}

/// The fixture through the real pipeline: the rewrite, the renderer,
/// the preview.
fn preview_of(name: &str) -> yagna_core::mdrender::Preview {
    let text = fixture(name);
    let (rewritten, entries) = rewrite_assets(text, &base());
    preview(&rewritten, &Fixed::new(&entries))
}

#[test]
fn the_preview_never_shows_the_raw_markdown_syntax() {
    for (name, _) in CORPUS {
        let visible = visible_text(&preview_of(name).blocks);
        assert!(
            !visible.contains("]("),
            "{name}: the raw link syntax never reaches the reader: {visible}"
        );
        assert!(
            !visible.contains("!["),
            "{name}: the raw image syntax never reaches the reader: {visible}"
        );
    }
}

#[test]
fn the_badge_wall_is_counted_and_never_drawn() {
    let text = fixture("badge-heavy.md");
    let (_, entries) = rewrite_assets(text, &base());
    let badges = entries.iter().filter(|entry| entry.badge).count();
    assert!(badges > 0, "the fixture is badge-heavy");
    let preview = preview_of("badge-heavy.md");
    assert_eq!(preview.badges, badges, "every badge is counted");
    assert_eq!(
        count_badges(&preview.blocks),
        0,
        "no badge image is drawn: {:?}",
        preview.blocks
    );
}

#[test]
fn the_words_of_the_readme_are_what_the_reader_sees() {
    let visible = visible_text(&preview_of("badge-heavy.md").blocks);
    assert!(
        visible.contains("Everything") && visible.contains("7.9"),
        "the table's words are what shows: {visible}"
    );
    assert!(
        !visible.contains("img.shields.io"),
        "no badge URL is what shows: {visible}"
    );
    assert!(
        visible.contains("cargo install everything"),
        "the code block keeps its text: {visible}"
    );
}

#[test]
fn a_plain_readme_still_renders_its_text() {
    let preview = preview_of("plain.md");
    assert_eq!(preview.badges, 0, "there is no badge to count");
    let visible = visible_text(&preview.blocks);
    assert!(
        visible.contains("Everything is one binary"),
        "the prose is what shows: {visible}"
    );
    assert!(
        visible.contains("the guide"),
        "the link shows its own text: {visible}"
    );
}
