//! The markdown fixture corpus: table-driven tests over the recorded
//! README shapes, the one place the markdown math meets real fixture
//! files. The fixtures are package data — embedded at compile time via
//! `include_str!`, never a repository read at run time.
//!
//! The corpus: plain markdown images, HTML img tags, relative paths,
//! query-stringed URLs, gifs, badges mixed with real images, a README
//! with no images, an empty README.

use yagna_core::markdown::{RawBase, image_refs, is_badge, readme_excerpt, rewrite_assets};

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

fn raw(path: &str) -> String {
    format!("https://raw.githubusercontent.com/someone/something/main/{path}")
}

#[test]
fn invariant_markdown_rewrite_is_deterministic() {
    for (name, _) in CORPUS {
        let text = fixture(name);
        let base = base();
        let first = rewrite_assets(text, &base);
        let second = rewrite_assets(text, &base);
        assert_eq!(first.0, second.0, "{name}: the rewrite repeats itself");
        assert_eq!(first.1, second.1, "{name}: the asset list repeats itself");
        for entry in &first.1 {
            assert_eq!(
                entry.image.asset_id,
                yagna_core::model::ImageRef::from_url(entry.image.url.clone()).asset_id,
                "{name}: the asset id is the url's blake3"
            );
        }
    }
}

#[test]
fn plain_images_resolve_and_rewrite() {
    let text = fixture("plain-images.md");
    let refs = image_refs(text, &base());
    let urls: Vec<&str> = refs.iter().map(|image| image.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            raw("docs/screen.png"),
            "https://cdn.example.org/hero.jpg".to_string()
        ]
    );

    let (rewritten, assets) = rewrite_assets(text, &base());
    assert_eq!(assets.len(), 2);
    assert!(!rewritten.contains("docs/screen.png"));
    assert!(!rewritten.contains("https://cdn.example.org/hero.jpg"));
    for entry in &assets {
        assert!(
            rewritten.contains(&format!("yagna-asset://{}", entry.image.asset_id)),
            "the destination became its asset id"
        );
    }
    assert!(!assets.iter().any(|entry| entry.badge));
}

#[test]
fn html_img_tags_extract_across_quoting_styles() {
    let text = fixture("html-img.md");
    let refs = image_refs(text, &base());
    let urls: Vec<&str> = refs.iter().map(|image| image.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            raw("assets/banner.png"),
            "https://media.example.org/demo.gif".to_string(),
            raw("t.png?w=100"),
        ]
    );
    let (rewritten, _) = rewrite_assets(text, &base());
    assert!(rewritten.contains("<img align=\"center\" src=\"yagna-asset://"));
    assert!(rewritten.contains("SRC=yagna-asset://"));
}

#[test]
fn relative_paths_normalize_against_the_raw_base() {
    let text = fixture("relative-paths.md");
    let urls: Vec<String> = image_refs(text, &base())
        .iter()
        .map(|image| image.url.clone())
        .collect();
    assert_eq!(
        urls,
        [
            raw("assets/a.png"),
            raw("assets/deep/b.png"),
            raw("root.png"),
            raw("assets/x.png"),
        ]
    );
}

#[test]
fn query_strings_survive_but_never_reach_the_output() {
    let text = fixture("query-strings.md");
    let refs = image_refs(text, &base());
    assert_eq!(refs.len(), 1);
    assert!(refs[0].url.ends_with("?raw=true&token=abc"));
    let (rewritten, assets) = rewrite_assets(text, &base());
    assert!(!rewritten.contains("?raw=true"));
    assert_eq!(assets.len(), 1);
}

#[test]
fn badges_are_marked_and_the_excerpt_is_text() {
    let text = fixture("badges-mixed.md");
    let assets = rewrite_assets(text, &base()).1;
    assert_eq!(assets.len(), 4);
    assert_eq!(assets.iter().filter(|entry| entry.badge).count(), 3);
    assert!(
        !assets
            .iter()
            .any(|entry| entry.badge && entry.image.url.ends_with("screenshot.png"))
    );

    let excerpt = readme_excerpt(text);
    assert!(
        excerpt.starts_with("A terminal recorder"),
        "the excerpt is the first paragraph, not badge alt text: {excerpt}"
    );
    assert!(!excerpt.contains("build"), "no badge alt text leaked");
}

#[test]
fn imageless_readmes_rewrite_to_themselves() {
    for name in ["no-images.md", "empty.md"] {
        let text = fixture(name);
        let base = base();
        assert!(image_refs(text, &base).is_empty(), "{name}");
        let (rewritten, assets) = rewrite_assets(text, &base);
        assert_eq!(rewritten, text, "{name}: nothing to rewrite");
        assert!(assets.is_empty(), "{name}");
    }
    assert_eq!(readme_excerpt(fixture("empty.md")), "");
}

#[test]
fn the_excerpt_of_the_plain_corpus_is_its_first_paragraph() {
    let excerpt = readme_excerpt(fixture("plain-images.md"));
    assert_eq!(excerpt, "A tool that does one thing well.");
}

#[test]
fn badge_detection_agrees_with_the_corpus() {
    assert!(is_badge(
        "https://img.shields.io/badge/build-passing-brightgreen"
    ));
    assert!(is_badge("https://badgen.net/crates/v/tool"));
    assert!(is_badge("https://travis-ci.com/owner/repo.svg?branch=main"));
    assert!(!is_badge("https://cdn.example.org/hero.jpg"));
}
