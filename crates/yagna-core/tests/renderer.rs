//! The renderer over the markdown fixture corpus: the README shapes
//! Part03 recorded, read back by the renderer Part06 wrote.
//!
//! The corpus: fixture file name and what the table asserts about it.
//! The fixtures are package data — embedded at compile time via
//! `include_str!`, never a repository read at run time.

use std::path::PathBuf;

use yagna_core::markdown::{RawBase, rewrite_assets};
use yagna_core::mdrender::{AssetResolver, Block, Inline, Resolved, render};

/// The corpus: fixture file name and what the table asserts about it.
const CORPUS: [(&str, &str); 8] = [
    ("plain-images.md", "two images, one relative one absolute"),
    ("html-img.md", "three img tags across quoting styles"),
    ("relative-paths.md", "four relative shapes, all normalized"),
    ("query-strings.md", "the query string survives in the url"),
    ("gifs.md", "a gif is an image like any other"),
    ("badges-mixed.md", "three badges and one real screenshot"),
    ("no-images.md", "prose only, nothing to rewrite"),
    ("empty.md", "nothing at all"),
];

fn fixture(name: &str) -> &'static str {
    let path = format!("markdown_fixtures/{name}");
    match path.as_str() {
        "markdown_fixtures/plain-images.md" => include_str!("markdown_fixtures/plain-images.md"),
        "markdown_fixtures/html-img.md" => include_str!("markdown_fixtures/html-img.md"),
        "markdown_fixtures/relative-paths.md" => {
            include_str!("markdown_fixtures/relative-paths.md")
        }
        "markdown_fixtures/query-strings.md" => include_str!("markdown_fixtures/query-strings.md"),
        "markdown_fixtures/gifs.md" => include_str!("markdown_fixtures/gifs.md"),
        "markdown_fixtures/badges-mixed.md" => include_str!("markdown_fixtures/badges-mixed.md"),
        "markdown_fixtures/no-images.md" => include_str!("markdown_fixtures/no-images.md"),
        "markdown_fixtures/empty.md" => include_str!("markdown_fixtures/empty.md"),
        other => panic!("unknown fixture {other}"),
    }
}

fn base() -> RawBase {
    RawBase {
        owner: "someone".to_string(),
        repo: "something".to_string(),
        branch: "main".to_string(),
    }
}

/// A resolver with a fixed answer per asset id.
struct Fixed {
    cached: Vec<String>,
}

impl Fixed {
    fn new(cached: &[&str]) -> Self {
        Self {
            cached: cached.iter().map(|id| id.to_string()).collect(),
        }
    }
}

impl AssetResolver for Fixed {
    fn resolve(&self, asset_id: &str) -> Resolved {
        if self.cached.iter().any(|id| id == asset_id) {
            return Resolved::File(PathBuf::from(format!("/cache/{asset_id}.img")));
        }
        if asset_id.starts_with("pending") {
            return Resolved::Pending;
        }
        Resolved::Missing
    }
}

/// Every image the blocks carry, in order.
fn images_of(blocks: &[Block]) -> Vec<(String, Resolved)> {
    let mut found = Vec::new();
    for block in blocks {
        let inlines = match block {
            Block::Paragraph(inlines) => inlines,
            Block::Heading { inlines, .. } => inlines,
            _ => continue,
        };
        for inline in inlines {
            if let Inline::Image { alt, asset } = inline {
                found.push((alt.clone(), asset.clone()));
            }
        }
    }
    found
}

#[test]
fn the_corpus_renders_without_panicking() {
    for (name, _) in CORPUS {
        let text = fixture(name);
        let (rewritten, _) = rewrite_assets(text, &base());
        let blocks = render(&rewritten, &Fixed::new(&[]));
        assert!(
            !blocks.is_empty() || text.trim().is_empty(),
            "{name}: the corpus renders into blocks"
        );
    }
}

#[test]
fn the_rewritten_corpus_resolves_its_images() {
    for (name, _) in CORPUS {
        let text = fixture(name);
        let (rewritten, assets) = rewrite_assets(text, &base());
        let ids: Vec<&str> = assets
            .iter()
            .map(|entry| entry.image.asset_id.as_str())
            .collect();
        let resolver = Fixed::new(&ids);
        let blocks = render(&rewritten, &resolver);
        for (alt, asset) in images_of(&blocks) {
            assert!(
                matches!(asset, Resolved::File(_)),
                "{name}: the image {alt:?} resolved to a file"
            );
        }
    }
}

#[test]
fn the_three_asset_states_are_answered() {
    let body = "![one](yagna-asset://cached)\n\n![two](yagna-asset://pending-1)\n\n![three](yagna-asset://gone)\n";
    let resolver = Fixed::new(&["cached"]);
    let blocks = render(body, &resolver);
    let states: Vec<Resolved> = images_of(&blocks)
        .into_iter()
        .map(|(_, asset)| asset)
        .collect();
    assert_eq!(
        states,
        [
            Resolved::File(PathBuf::from("/cache/cached.img")),
            Resolved::Pending,
            Resolved::Missing,
        ]
    );
}

#[test]
fn raw_html_is_stripped_to_text() {
    let text = fixture("html-img.md");
    let (rewritten, _) = rewrite_assets(text, &base());
    let blocks = render(&rewritten, &Fixed::new(&[]));
    let rendered = format!("{blocks:?}");
    assert!(
        !rendered.contains("<img"),
        "no raw tag survives into the blocks: {rendered}"
    );
    assert!(
        !rendered.contains("align="),
        "no attribute survives into the blocks: {rendered}"
    );
}

#[test]
fn the_empty_readme_renders_nothing() {
    assert!(render(fixture("empty.md"), &Fixed::new(&[])).is_empty());
}
