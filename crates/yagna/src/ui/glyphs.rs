//! The built-in assets: the shapes the window draws itself, served to
//! the UI framework as assets.
//!
//! The framework's `svg` element reads an SVG document by path through
//! the application's asset source, and it applies a transformation to
//! the rendered shape — that is what makes the row's chevron rotate.
//! One document is enough for the whole app, so the source serves it
//! and nothing else. The document's fill is the theme's ink, filled in
//! when the document is served — the color flows through the applier
//! like every other.
//!
//! The curated icons come the same way: each is embedded at compile
//! time and served under its own path. The framework rasterises an SVG
//! into an alpha mask and paints that mask with the element's own ink,
//! so a document's own color never reaches the screen — only its
//! shape does.
//!
//! Items: `BuiltinAssets` — the asset source; `CHEVRON_ASSET` — the
//! path the chevron is served under; `BRAND_ASSETS` — the paths the
//! two brand marks are served under.
//! Depends on: gpui (the asset source trait), crate::ui::theme (the
//! ink).
//! Called by: crate::ui (the window's asset registration).

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

use super::theme;

/// The path the chevron's document is served under.
pub(crate) const CHEVRON_ASSET: &str = "yagna-chevron.svg";

/// The path the product mark is served under.
pub(crate) const BRAND_MARK_ASSET: &str = "yagna-brand/yagna.png";

/// The path the author mark is served under.
pub(crate) const BRAND_AUTHOR_ASSET: &str = "yagna-brand/ag-logo.png";

/// The chevron: a filled triangle pointing down, in a ten-unit box. The
/// framework paints it with the row's ink and rotates it as the row
/// expands; `{ink}` is that ink, filled in at serve time.
const CHEVRON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10"><path d="M1 3 L9 3 L5 8 Z" fill="{ink}"/></svg>"##;

/// The icon documents, embedded at compile time. Each is the upstream
/// outline or filled shape; the source substitutes the ink for
/// `currentColor` and widens the outline's stroke so it lands at the
/// icon's drawn size.
const ICON_DOCUMENTS: [(&str, &str); 18] = [
    (
        "yagna-icon/alert-triangle.svg",
        include_str!("assets/yagna-icon/alert-triangle.svg"),
    ),
    (
        "yagna-icon/arrow-up-right.svg",
        include_str!("assets/yagna-icon/arrow-up-right.svg"),
    ),
    (
        "yagna-icon/book-filled.svg",
        include_str!("assets/yagna-icon/book-filled.svg"),
    ),
    (
        "yagna-icon/book.svg",
        include_str!("assets/yagna-icon/book.svg"),
    ),
    (
        "yagna-icon/check.svg",
        include_str!("assets/yagna-icon/check.svg"),
    ),
    (
        "yagna-icon/chevron-down.svg",
        include_str!("assets/yagna-icon/chevron-down.svg"),
    ),
    (
        "yagna-icon/compass.svg",
        include_str!("assets/yagna-icon/compass.svg"),
    ),
    (
        "yagna-icon/filter.svg",
        include_str!("assets/yagna-icon/filter.svg"),
    ),
    (
        "yagna-icon/refresh.svg",
        include_str!("assets/yagna-icon/refresh.svg"),
    ),
    (
        "yagna-icon/search.svg",
        include_str!("assets/yagna-icon/search.svg"),
    ),
    (
        "yagna-icon/settings-filled.svg",
        include_str!("assets/yagna-icon/settings-filled.svg"),
    ),
    (
        "yagna-icon/settings.svg",
        include_str!("assets/yagna-icon/settings.svg"),
    ),
    (
        "yagna-icon/star-filled.svg",
        include_str!("assets/yagna-icon/star-filled.svg"),
    ),
    (
        "yagna-icon/star.svg",
        include_str!("assets/yagna-icon/star.svg"),
    ),
    (
        "yagna-icon/tag.svg",
        include_str!("assets/yagna-icon/tag.svg"),
    ),
    (
        "yagna-icon/user.svg",
        include_str!("assets/yagna-icon/user.svg"),
    ),
    (
        "yagna-icon/users.svg",
        include_str!("assets/yagna-icon/users.svg"),
    ),
    ("yagna-icon/x.svg", include_str!("assets/yagna-icon/x.svg")),
];

/// The product mark: the window's own mark, drawn in the shell's
/// header and the About card.
const BRAND_MARK: &[u8] = include_bytes!("../../assets/brand/yagna.png");

/// The author mark: the small mark that says who built it.
const BRAND_AUTHOR: &[u8] = include_bytes!("../../assets/brand/ag-logo.png");

/// The application's asset source: the built-in documents, and nothing
/// else. A path it does not know yields nothing, which the framework
/// paints as a missing shape rather than as an error.
pub(crate) struct BuiltinAssets;

impl AssetSource for BuiltinAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == CHEVRON_ASSET {
            let document = CHEVRON_SVG.replace("{ink}", &theme::text_hex());
            return Ok(Some(Cow::Owned(document.into_bytes())));
        }
        if path == BRAND_MARK_ASSET {
            return Ok(Some(Cow::Borrowed(BRAND_MARK)));
        }
        if path == BRAND_AUTHOR_ASSET {
            return Ok(Some(Cow::Borrowed(BRAND_AUTHOR)));
        }
        if let Some((_, document)) = ICON_DOCUMENTS.iter().find(|(served, _)| *served == path) {
            let tinted = tint_icon(document);
            return Ok(Some(Cow::Owned(tinted.into_bytes())));
        }
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// The ink an icon document is served with: the theme's text ink, in
/// place of the document's `currentColor`.
fn tint_icon(document: &str) -> String {
    document.replace("currentColor", &theme::text_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chevron is served, and nothing else is served under its
    /// path.
    #[test]
    fn the_chevron_is_served_and_nothing_else_is() {
        let assets = BuiltinAssets;
        let served = assets.load(CHEVRON_ASSET).expect("load");
        let served = served.expect("the chevron is there");
        let text = String::from_utf8(served.to_vec()).expect("utf-8");
        assert!(text.contains("<svg"), "the document is an SVG: {text}");
        assert!(text.contains("viewBox"), "it carries its own box");
        assert!(assets.load("something-else").expect("load").is_none());
        assert!(assets.list("").expect("list").is_empty());
    }

    /// Every curated icon is served under its own path, and the
    /// document carries the ink instead of `currentColor`.
    #[test]
    fn every_icon_is_served_tinted() {
        let assets = BuiltinAssets;
        for (path, _) in ICON_DOCUMENTS {
            let served = assets.load(path).expect("load");
            let served = served.expect("the icon is there");
            let text = String::from_utf8(served.to_vec()).expect("utf-8");
            assert!(text.contains("<svg"), "{path} is an SVG");
            assert!(
                !text.contains("currentColor"),
                "{path} carries the ink, not the keyword"
            );
        }
    }

    /// The two brand marks are served as the bytes they were embedded
    /// with.
    #[test]
    fn the_brand_marks_are_served() {
        let assets = BuiltinAssets;
        let mark = assets.load(BRAND_MARK_ASSET).expect("load").expect("there");
        assert_eq!(mark.to_vec(), BRAND_MARK.to_vec());
        let author = assets
            .load(BRAND_AUTHOR_ASSET)
            .expect("load")
            .expect("there");
        assert_eq!(author.to_vec(), BRAND_AUTHOR.to_vec());
        // A PNG: the signature every reader recognizes.
        assert_eq!(&BRAND_MARK[..4], b"\x89PNG");
        assert_eq!(&BRAND_AUTHOR[..4], b"\x89PNG");
    }
}
