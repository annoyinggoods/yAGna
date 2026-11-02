//! The pure README math: image extraction, badge detection, the
//! offline rewrite, and the excerpt.
//!
//! `image_refs` and `rewrite_assets` find every image in a README —
//! markdown `![alt](url)` and HTML `<img src>` — and resolve relative
//! destinations against the repository's raw URL base. The rewrite
//! replaces each destination with `yagna-asset://<id>`; the id is the
//! URL's blake3, so the rewrite is deterministic by construction (an
//! `invariant_` test pins it). Badges are marked, never downloaded;
//! the excerpt is the README's first readable paragraph.
//!
//! Items: `RawBase` — the raw-URL resolution base; `AssetEntry` — one
//! asset the rewrite produced (image plus its badge flag);
//! `image_refs`, `is_badge`, `rewrite_assets`, `readme_excerpt`,
//! `carries_svg_filter` — the public math; `html_attribute_span` — one
//! HTML tag attribute's value span; `ImageSpan`, `image_spans` — the
//! shared scanner.
//! Depends on: crate::model (ImageRef, the deterministic asset id).
//! Called by: yagna_store (the README pipeline), crate::mdrender (the
//! renderer) and the app's README rendering.

use std::collections::HashSet;

use crate::model::ImageRef;

/// The markdown protocol an image destination is rewritten to. The
/// renderer resolves it against the store's asset cache — never the
/// network.
pub const ASSET_SCHEME: &str = "yagna-asset://";

/// The raw-URL resolution base: owner, repository, branch. Built from a
/// snapshot's `default_branch` — a repository with no branch has no
/// README to resolve images for either (the caller substitutes `HEAD`
/// when it must still store one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBase {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

impl RawBase {
    /// The base URL every relative image resolves against, with the
    /// trailing slash in place for plain joining.
    pub fn raw_root(&self) -> String {
        format!(
            "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/",
            owner = self.owner,
            repo = self.repo,
            branch = self.branch
        )
    }
}

/// One asset the rewrite found: the image (URL plus content-hash id)
/// and whether the badge table claims it. The seed row for the store's
/// asset table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    pub image: ImageRef,
    pub badge: bool,
}

/// One image occurrence in the source text: the destination's byte span
/// (for in-place rewriting), the whole construct's span (for removal),
/// and the destination's raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageSpan {
    dest_start: usize,
    dest_end: usize,
    whole_start: usize,
    whole_end: usize,
    text: String,
}

/// Every image in a README, resolved and deduplicated in order of
/// first appearance. Repeated URLs collapse into one ref — the same
/// screenshot twice is one asset.
pub fn image_refs(md: &str, base: &RawBase) -> Vec<ImageRef> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for span in image_spans(md) {
        let Some(url) = resolve_destination(&span.text, base) else {
            continue;
        };
        if seen.insert(url.clone()) {
            refs.push(ImageRef::from_url(url));
        }
    }
    refs
}

/// The badge table: URL fragments that identify CI/status badges. A
/// badge is marked and never downloaded — the row preview picks the
/// first non-badge image.
const BADGE_MARKERS: [&str; 14] = [
    "shields.io",
    "badgen.net",
    "/badge/",
    "travis-ci.com",
    "travis-ci.org",
    "api.codacy.com",
    "coveralls.io",
    "badge.fury.io",
    "snyk.io",
    "sonarcloud.io",
    "circleci.com",
    "codecov.io",
    "codeclimate.com",
    "opencollective.com",
];

/// Whether the bytes are an SVG carrying a filter element.
///
/// Such a file makes the SVG rasteriser abort the process — an
/// assertion in its filter pipeline, on a thread that cannot unwind.
/// The bytes of a README image come from someone else's host, so the
/// fact is worth knowing before the file is handed to a rasteriser.
pub fn carries_svg_filter(bytes: &[u8]) -> bool {
    contains(bytes, b"<svg") && contains(bytes, b"<filter")
}

/// Whether the bytes hold the needle.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// True when the URL names a badge service, compared case-insensitively.
pub fn is_badge(url: &str) -> bool {
    let lower = url.to_lowercase();
    BADGE_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// The offline rewrite: every image destination becomes
/// `yagna-asset://<asset_id>`, and every destination becomes an
/// `AssetEntry` (deduplicated, first-appearance order). Same input
/// twice — byte-identical output and identical ids.
pub fn rewrite_assets(md: &str, base: &RawBase) -> (String, Vec<AssetEntry>) {
    let mut rewritten = String::with_capacity(md.len());
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = 0;
    for span in image_spans(md) {
        let Some(url) = resolve_destination(&span.text, base) else {
            continue;
        };
        let image = ImageRef::from_url(url);
        let badge = is_badge(&image.url);
        rewritten.push_str(&md[cursor..span.dest_start]);
        rewritten.push_str(ASSET_SCHEME);
        rewritten.push_str(&image.asset_id);
        cursor = span.dest_end;
        if seen.insert(image.url.clone()) {
            entries.push(AssetEntry { image, badge });
        }
    }
    rewritten.push_str(&md[cursor..]);
    (rewritten, entries)
}

/// The excerpt cap: past this the paragraph ends at the last word
/// boundary with an ellipsis.
const EXCERPT_CAP: usize = 400;

/// The README's first readable paragraph: headings, code fences, HTML
/// blocks, and image-only lines (badge walls) are skipped; whitespace
/// collapses; the result is capped at ~400 characters. The value the
/// list row renders as its two italic lines.
pub fn readme_excerpt(original: &str) -> String {
    let mut paragraph = String::new();
    let mut fenced = false;
    for line in original.lines() {
        match excerpt_step(&mut fenced, &mut paragraph, line) {
            Step::Done(text) => return text,
            Step::Skip => continue,
            Step::Text => {}
        }
        paragraph.push_str(line.trim());
        paragraph.push(' ');
    }
    paragraph_text(&paragraph)
        .map(|text| cap_excerpt(&text))
        .unwrap_or_default()
}

/// One line's effect on the excerpt walk.
enum Step {
    /// The line is paragraph text: append it.
    Text,
    /// The line belongs to no paragraph (inside a fence, or a boundary
    /// that found nothing): it is consumed and the walk moves on.
    Skip,
    /// The walk is finished — this is the excerpt.
    Done(String),
}

/// One line's step in the excerpt walk: fences toggle and interrupt,
/// blank lines end a paragraph, any other line is paragraph text.
fn excerpt_step(fenced: &mut bool, paragraph: &mut String, line: &str) -> Step {
    let trimmed = line.trim();
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        return fence_toggle(fenced, paragraph);
    }
    if *fenced {
        return Step::Skip;
    }
    if trimmed.is_empty() {
        return blank_boundary(paragraph);
    }
    Step::Text
}

/// The fence toggle: a fence that *opens* on a finished paragraph hands
/// back that paragraph — a fence interrupts a paragraph, it does not
/// discard it.
fn fence_toggle(fenced: &mut bool, paragraph: &mut String) -> Step {
    let opening = !*fenced;
    *fenced = opening;
    let found = opening.then(|| paragraph_text(paragraph)).flatten();
    paragraph.clear();
    found.map_or(Step::Skip, |text| Step::Done(cap_excerpt(&text)))
}

/// A blank line: the accumulated paragraph, when it held one, is the
/// excerpt.
fn blank_boundary(paragraph: &mut String) -> Step {
    let found = paragraph_text(paragraph);
    paragraph.clear();
    found.map_or(Step::Skip, |text| Step::Done(cap_excerpt(&text)))
}

/// The readable text accumulated so far, once it holds any: images and
/// link scaffolding stripped, headings and tag-only lines rejected.
fn paragraph_text(paragraph: &str) -> Option<String> {
    if paragraph.trim_start().starts_with('#') {
        return None;
    }
    let unlinked = strip_link_markup(&strip_image_markup(paragraph));
    let plain = strip_html_tags(&unlinked);
    let collapsed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    (collapsed.chars().count() >= 3).then_some(collapsed)
}

/// Removes markdown link scaffolding — `[label](url)` becomes `label` —
/// so an image wrapped in a link cannot leak its brackets. Runs after
/// the image strip.
fn strip_link_markup(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(close) = rest.find("](") else {
            kept.push_str(rest);
            return kept;
        };
        let Some(open) = rest[..close].rfind('[') else {
            kept.push_str(&rest[..close + 2]);
            rest = &rest[close + 2..];
            continue;
        };
        let Some(paren_close) = matching_paren(&rest[close + 1..]) else {
            kept.push_str(rest);
            return kept;
        };
        kept.push_str(&rest[..open]);
        kept.push_str(&rest[open + 1..close]);
        rest = &rest[close + paren_close + 2..];
    }
}

/// The cap: cut at the last word boundary inside the limit (a character
/// limit — a CJK README is not a third of a Latin one), ellipsis
/// appended.
fn cap_excerpt(text: &str) -> String {
    if text.chars().count() <= EXCERPT_CAP {
        return text.to_string();
    }
    let cut = text
        .char_indices()
        .nth(EXCERPT_CAP)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let head = &text[..cut];
    match head.rfind(' ') {
        Some(boundary) => format!("{}…", &head[..boundary]),
        None => format!("{head}…"),
    }
}

/// Removes image constructs (markdown and HTML) from a text entirely —
/// alt text included, so a badge wall cannot speak through the excerpt.
fn strip_image_markup(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in image_spans(text) {
        kept.push_str(&text[cursor..span.whole_start]);
        cursor = span.whole_end;
    }
    kept.push_str(&text[cursor..]);
    kept
}

/// Removes `<tag>` markup, keeping the text between tags.
fn strip_html_tags(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        kept.push_str(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    kept.push_str(rest);
    kept
}

/// Every image occurrence in the text, in order: markdown images and
/// HTML `<img>` src attributes, each as byte spans plus destination.
///
/// The two scanners see the same bytes, so a construct of one kind can
/// sit inside a construct of the other (a markdown destination holding
/// an `<img src=…>`, or the reverse). The spans are ordered outermost
/// first, and any span that overlaps the construct already kept is
/// dropped — the outer construct owns those bytes, and the callers'
/// cursors never move backwards.
fn image_spans(md: &str) -> Vec<ImageSpan> {
    let mut spans = Vec::new();
    markdown_image_spans(md, &mut spans);
    html_image_spans(md, &mut spans);
    spans.sort_by(|left, right| {
        left.whole_start
            .cmp(&right.whole_start)
            .then(right.whole_end.cmp(&left.whole_end))
    });
    let mut kept = Vec::new();
    let mut end = 0usize;
    for span in spans {
        if span.whole_start < end {
            continue;
        }
        end = span.whole_end;
        kept.push(span);
    }
    kept
}

/// The markdown form: `![alt](destination "title")`. The destination
/// runs to the closing parenthesis, honoring nested parens; a title is
/// split off at the first whitespace. The whole construct starts at the
/// `!`.
fn markdown_image_spans(md: &str, spans: &mut Vec<ImageSpan>) {
    let bytes = md.as_bytes();
    let mut at = 0;
    while let Some(bang) = find_byte(&bytes[at..], b'!') {
        let start = at + bang;
        at = start + 1;
        if bytes.get(start + 1) != Some(&b'[') {
            continue;
        }
        let Some(dest) = markdown_destination(&md[start..]) else {
            continue;
        };
        spans.push(ImageSpan {
            dest_start: start + dest.open,
            dest_end: start + dest.close,
            whole_start: start,
            whole_end: start + dest.close + 1,
            text: dest.text,
        });
        at = start + dest.close;
    }
}

/// The destination of one markdown image relative to its `!`: the span
/// between the parentheses (excluded) and the cleaned text.
struct MdDestination {
    open: usize,
    close: usize,
    text: String,
}

fn markdown_destination(rest: &str) -> Option<MdDestination> {
    let bracket = rest.find(']')?;
    let open = bracket + 1;
    if !rest[open..].starts_with('(') {
        return None;
    }
    let close = matching_paren(&rest[open..])? + open;
    let raw = rest[open + 1..close].trim();
    if raw.is_empty() {
        return None;
    }
    let text = raw
        .split_whitespace()
        .next()?
        .trim_matches(|edge| edge == '<' || edge == '>')
        .to_string();
    Some(MdDestination {
        open: open + 1,
        close,
        text,
    })
}

/// The index (relative to `rest`) of the `)` closing the parenthesis at
/// position 0, honoring nested parentheses. The one implementation of
/// the rule — the renderer's inline parser asks for it too.
pub(crate) fn matching_paren(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The HTML form: `<img … src="…">` (single, double, or unquoted). The
/// span covers the attribute value only. The scan is ASCII-case
/// insensitive over the original text, so spans stay valid.
fn html_image_spans(md: &str, spans: &mut Vec<ImageSpan>) {
    let mut at = 0;
    while let Some(found) = find_ascii_ci(&md[at..], "<img") {
        let tag_start = at + found;
        let Some(tag_end) = html_tag_end(md, tag_start) else {
            return;
        };
        at = tag_end;
        if let Some((value_start, value_end)) = src_value_span(&md[tag_start..tag_end]) {
            spans.push(ImageSpan {
                dest_start: tag_start + value_start,
                dest_end: tag_start + value_end,
                whole_start: tag_start,
                whole_end: tag_end,
                text: md[tag_start + value_start..tag_start + value_end].to_string(),
            });
        }
    }
}

/// The byte index just past a tag's closing `>`, honoring quoted
/// attribute values (`>` inside quotes does not close the tag).
pub(crate) fn html_tag_end(text: &str, tag_start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote = 0u8;
    for (offset, byte) in bytes[tag_start..].iter().enumerate() {
        match (quote, *byte) {
            (0, b'>') => return Some(tag_start + offset + 1),
            (0, b'"') | (0, b'\'') => quote = *byte,
            (open_quote, byte) if open_quote == byte => quote = 0,
            _ => {}
        }
    }
    None
}

/// The `src` attribute's value span inside one tag body, relative to the
/// tag's start. The scan text is lowercased; quotes delimit the value
/// when present, whitespace ends it otherwise.
fn src_value_span(tag: &str) -> Option<(usize, usize)> {
    html_attribute_span(tag, "src")
}

/// The span of one attribute's value inside a tag body — the value's
/// bounds relative to the tag's start, with the quotes excluded. The
/// name must be preceded by whitespace, so `data-src=` never matches
/// `src=`; the name itself is ASCII-case insensitive. `None` when the
/// attribute is absent or carries no value.
pub fn html_attribute_span(tag: &str, name: &str) -> Option<(usize, usize)> {
    let value_start = attribute_value_start(tag, name)?;
    let rest = &tag[value_start..];
    match rest.as_bytes().first()? {
        quote @ (b'"' | b'\'') => {
            let close = rest[1..].find(*quote as char)? + 1;
            Some((value_start + 1, value_start + close))
        }
        _ => {
            let end = rest
                .find(|char: char| char.is_whitespace() || char == '>')
                .unwrap_or(rest.len());
            (end > 0).then_some((value_start, value_start + end))
        }
    }
}

/// Where an attribute's value starts inside a tag body: after `name=`
/// and any whitespace. The character before the name must be whitespace,
/// so `data-src=` does not match `src=`. The name search is ASCII-case
/// insensitive.
fn attribute_value_start(tag: &str, name: &str) -> Option<usize> {
    let pattern = format!("{name}=");
    let mut search = 0;
    while let Some(found) = find_ascii_ci(&tag[search..], &pattern) {
        let at = search + found;
        let preceded_by_space = at > 0 && tag[..at].ends_with(char::is_whitespace);
        if preceded_by_space {
            let value = at + pattern.len();
            let gap = tag[value..].len() - tag[value..].trim_start().len();
            return Some(value + gap);
        }
        search = at + name.len();
    }
    None
}

/// The first byte offset of `needle` in `haystack` ignoring ASCII case;
/// multibyte characters pass through untouched, so offsets stay valid.
pub fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let hay = haystack.as_bytes();
    let nee = needle.as_bytes();
    if nee.is_empty() || hay.len() < nee.len() {
        return None;
    }
    (0..=hay.len() - nee.len()).find(|&at| hay[at..at + nee.len()].eq_ignore_ascii_case(nee))
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|byte| *byte == needle)
}

/// Resolves one image destination to its absolute URL. Absolute
/// http(s) URLs pass through untouched (query strings stay intact);
/// inline `data:` URIs and unknown schemes are not assets; everything
/// else resolves against the repository's raw base.
fn resolve_destination(dest: &str, base: &RawBase) -> Option<String> {
    let trimmed = dest.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if lower.starts_with("data:") {
        return None;
    }
    if lower.starts_with("//") {
        return Some(format!("https:{trimmed}"));
    }
    if trimmed.contains(':') {
        return None;
    }
    Some(join_relative(trimmed, base))
}

/// Joins a relative path onto the raw base: a leading `/` is
/// repository-root relative, `..`/`.` segments normalize, and the query
/// string rides along untouched (some CDNs need it).
fn join_relative(dest: &str, base: &RawBase) -> String {
    let without_fragment = dest
        .split_once('#')
        .map(|(before, _)| before)
        .unwrap_or(dest);
    let (path_part, query) = match without_fragment.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (without_fragment, None),
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in path_part.split('/') {
        match segment {
            ".." => {
                segments.pop();
            }
            "." | "" => {}
            named => segments.push(named),
        }
    }
    let mut url = base.raw_root();
    url.push_str(&segments.join("/"));
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> RawBase {
        RawBase {
            owner: "someone".to_string(),
            repo: "something".to_string(),
            branch: "main".to_string(),
        }
    }

    #[test]
    fn an_svg_with_a_filter_is_recognised() {
        assert!(carries_svg_filter(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><filter id="f"><feDisplacementMap/></filter></svg>"#
        ));
        assert!(!carries_svg_filter(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"/></svg>"#
        ));
        assert!(
            !carries_svg_filter(b"\x89PNG\r\n\x1a\n not an svg at all"),
            "a raster image is never an SVG"
        );
        assert!(
            !carries_svg_filter(b"<html><body>filter</body></html>"),
            "the word in another markup is not a filter"
        );
    }

    #[test]
    fn is_badge_matches_the_table_case_insensitively() {
        assert!(is_badge(
            "https://img.shields.io/badge/ci-passing-brightgreen"
        ));
        assert!(is_badge("https://badgen.net/github/release/x/y"));
        assert!(is_badge("https://travis-ci.com/owner/repo.svg?branch=main"));
        assert!(is_badge("https://api.codacy.com/project/badge/Grade/abc"));
        assert!(is_badge("https://some.host/badge/status.svg"));
        assert!(!is_badge(
            "https://raw.githubusercontent.com/o/r/main/docs/shot.png"
        ));
        assert!(!is_badge("https://cdn.example.org/badgercam/shot.png"));
    }

    #[test]
    fn excerpt_skips_headings_fences_and_picks_the_first_paragraph() {
        let md = "# Title\n\n![hero](pic.png)\n\n```rust\nlet code = here;\n```\n\nFirst real   paragraph\nwith a second line.\n\nAnother paragraph.";
        let excerpt = readme_excerpt(md);
        assert_eq!(excerpt, "First real paragraph with a second line.");
    }

    #[test]
    fn excerpt_caps_at_the_word_boundary() {
        let long = "word ".repeat(120);
        let excerpt = readme_excerpt(&long);
        assert!(excerpt.chars().count() < 410);
        assert!(excerpt.ends_with('…'));
        assert_eq!(readme_excerpt("tiny"), "tiny");
    }

    #[test]
    fn resolve_rejects_unknown_schemes_and_data_uris() {
        let base = base();
        assert_eq!(
            resolve_destination("data:image/png;base64,AAAA", &base),
            None
        );
        assert_eq!(resolve_destination("mailto:ops@example.org", &base), None);
        assert_eq!(resolve_destination("", &base), None);
        assert_eq!(
            resolve_destination("//cdn.example.org/pic.png", &base).as_deref(),
            Some("https://cdn.example.org/pic.png")
        );
    }

    #[test]
    fn relative_resolution_normalizes_segments_and_keeps_the_query() {
        let base = base();
        let resolved =
            resolve_destination("../assets/deep/../b.png?w=8#anchor", &base).expect("resolvable");
        assert_eq!(
            resolved,
            "https://raw.githubusercontent.com/someone/something/main/assets/b.png?w=8"
        );
        assert_eq!(
            resolve_destination("/root.png", &base).as_deref(),
            Some("https://raw.githubusercontent.com/someone/something/main/root.png")
        );
    }

    #[test]
    fn html_spans_handle_quoted_unquoted_and_data_src() {
        let base = base();
        let md = "<img src=\"a.png\"> <img SRC=b.png> <img data-src=\"x.png\"> <img alt='>' src='c.png'>";
        let refs = image_refs(md, &base);
        let urls: Vec<&str> = refs.iter().map(|image| image.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://raw.githubusercontent.com/someone/something/main/a.png",
                "https://raw.githubusercontent.com/someone/something/main/b.png",
                "https://raw.githubusercontent.com/someone/something/main/c.png",
            ],
            "data-src is not an image; a > inside quotes does not close the tag"
        );
    }

    #[test]
    fn markdown_images_with_nested_parens_and_titles() {
        let base = base();
        let md = "![a](docs/pic_(1).png \"the title\") ![b](<angle.png>)";
        let refs = image_refs(md, &base);
        let urls: Vec<&str> = refs.iter().map(|image| image.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://raw.githubusercontent.com/someone/something/main/docs/pic_(1).png",
                "https://raw.githubusercontent.com/someone/something/main/angle.png",
            ]
        );
    }

    #[test]
    fn a_repeated_url_is_one_ref_one_entry() {
        let base = base();
        let md = "![a](same.png) ![b](same.png) <img src='same.png'>";
        assert_eq!(image_refs(md, &base).len(), 1);
        assert_eq!(rewrite_assets(md, &base).1.len(), 1);
    }

    #[test]
    fn overlapping_markdown_and_html_image_constructs_never_panic() {
        let base = base();
        // A markdown destination holding an <img> tag, and the reverse:
        // the outer construct wins, the inner one is dropped, and
        // neither the rewrite nor the excerpt moves a cursor backwards.
        let md_html_inside_md = "![a](<img src=\"b.png\">)";
        let (rewritten, entries) = rewrite_assets(md_html_inside_md, &base);
        assert_eq!(entries.len(), 1, "the outer markdown image wins");
        assert!(rewritten.contains(ASSET_SCHEME));
        let md_inside_html = "<img src=\"![a](b.png)\">";
        let (rewritten, entries) = rewrite_assets(md_inside_html, &base);
        assert_eq!(entries.len(), 1, "the outer HTML image wins");
        assert!(rewritten.contains(ASSET_SCHEME));
        assert_eq!(
            readme_excerpt("intro ![a](<img src=\"b.png\">) tail"),
            "intro tail"
        );
    }

    #[test]
    fn a_fence_interrupting_a_paragraph_keeps_the_paragraph() {
        let md = "Intro text\n```\ncode\n```\nAfter paragraph.";
        assert_eq!(readme_excerpt(md), "Intro text");
    }

    #[test]
    fn the_excerpt_cap_counts_characters_not_bytes() {
        let cjk = "字".repeat(600);
        let excerpt = readme_excerpt(&cjk);
        assert!(
            excerpt.chars().count() >= 390 && excerpt.chars().count() <= 401,
            "a CJK excerpt is ~400 characters, not ~400 bytes: {} chars",
            excerpt.chars().count()
        );
    }
}
