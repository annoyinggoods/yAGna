//! The rendered README: the core's blocks painted as elements.
//!
//! The renderer (the core) knows the shape; this module knows the
//! pixels. The contract between them is the asset resolver: an image
//! that is cached paints as the cached file, one that is on its way
//! paints as a placeholder, one that is not there paints as a quiet
//! marker, and a badge paints as a quiet marker too — never an error,
//! never a broken picture.
//!
//! One layout fact shapes the whole module: a plain `div` in gpui is
//! a block container whose children stack vertically, so inline runs
//! cannot be its direct children — a paragraph whose children were one
//! div per run would put every styled span on its own line. Every
//! inline-bearing container therefore flows its children in a wrapping
//! row with baseline alignment, and adjacent text runs are merged
//! into one child so words wrap inside a run, not between runs.
//!
//! Items: `TabResolver` — the tab's answer book; `PreviewResolver` —
//! the row preview's answer book; `ReadmeLayout` — the sizes one
//! surface paints the shapes at; `render_blocks` — the blocks as
//! elements; `render_preview_blocks` — the same blocks in a row;
//! `render_block`/`render_flow` — one block, one run of flowed
//! inlines; `Links` — the painter's unique link ids; `merge_text_runs`
//! — the wrap-friendly form of an inline run.
//! Depends on: yagna_core::mdrender (the blocks), yagna_core::markdown
//! (the badge flag), yagna_store (the asset records), gpui (the
//! elements), crate::ui::theme (the palette), crate::ui::typography
//! (the row's sizes).
//! Called by: crate::ui::repo (the tab's README section) and
//! crate::ui::rows (the expanded row's preview).

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, ObjectFit, SharedString, Window, div, img, prelude::*, px,
};
use yagna_core::markdown::AssetEntry;
use yagna_core::mdrender::{AssetResolver, Block, Inline, ListItem, Resolved};
use yagna_store::AssetRecord;

use super::assets;
use super::theme;
use super::typography::{self, TypeScale};

/// The README's type sizes: the headings step down, the body sits
/// between the row's small text and its name. The tab and the expanded
/// row paint the same shapes at different sizes — one renderer, two
/// layouts.
pub(crate) struct ReadmeLayout {
    /// The first heading.
    pub(crate) h1: gpui::Pixels,
    /// The second heading.
    pub(crate) h2: gpui::Pixels,
    /// The third heading.
    pub(crate) h3: gpui::Pixels,
    /// The fourth and further headings.
    pub(crate) h4: gpui::Pixels,
    /// The body text.
    pub(crate) body: gpui::Pixels,
    /// The code blocks and the code spans.
    pub(crate) code: gpui::Pixels,
    /// The image's height ceiling.
    pub(crate) image_height: gpui::Pixels,
    /// The image's width ceiling — an image scales down inside it, it
    /// is never cropped and never bleeds past the column.
    pub(crate) image_width: gpui::Pixels,
    /// The gap between the blocks.
    pub(crate) gap: gpui::Pixels,
}

impl ReadmeLayout {
    /// The tab's layout: the README as the repository tab shows it.
    pub(crate) fn tab() -> Self {
        Self {
            h1: px(20.0),
            h2: px(17.0),
            h3: px(15.0),
            h4: px(13.0),
            body: px(13.0),
            code: px(12.0),
            image_height: px(320.0),
            image_width: px(560.0),
            gap: px(10.0),
        }
    }

    /// The row's layout: the same README inside an expanded row, at the
    /// row's own sizes — a preview is a teaser, not a second reading
    /// surface.
    pub(crate) fn preview() -> Self {
        Self {
            h1: typography::scaled(TypeScale::TITLE),
            h2: typography::scaled(TypeScale::BODY),
            h3: typography::scaled(TypeScale::PREVIEW),
            h4: typography::scaled(TypeScale::PREVIEW),
            body: typography::scaled(TypeScale::PREVIEW),
            code: typography::scaled(12.0),
            image_height: px(160.0),
            image_width: px(400.0),
            gap: px(6.0),
        }
    }
}

/// The tab's resolver: the answers the tab worked out for the
/// repository's assets, given by id.
pub(crate) struct TabResolver<'a> {
    assets: &'a HashMap<String, Resolved>,
}

impl<'a> TabResolver<'a> {
    /// The answer book over the assets the tab resolved.
    pub(crate) fn new(assets: &'a HashMap<String, Resolved>) -> Self {
        Self { assets }
    }
}

impl AssetResolver for TabResolver<'_> {
    fn resolve(&self, asset_id: &str) -> Resolved {
        self.assets
            .get(asset_id)
            .cloned()
            .unwrap_or(Resolved::Missing)
    }
}

/// The preview's resolver: the answers for the images an expanded row
/// shows. A badge answers as a badge — the reader counts it instead of
/// looking at it. The row's own image and the repository's other
/// images answer with the store's own records, so a relative image
/// resolves to its cached file.
pub(crate) struct PreviewResolver {
    /// The answers, by asset id.
    answers: HashMap<String, Resolved>,
}

impl PreviewResolver {
    /// The answer book over a rewritten README: the badge ids are the
    /// rewrite's badge flags, the rest are the store's own answers.
    pub(crate) fn new(
        entries: &[AssetEntry],
        assets: &[AssetRecord],
        asset: Option<&AssetRecord>,
    ) -> Self {
        let mut stored: Vec<AssetRecord> = assets.to_vec();
        if let Some(asset) = asset
            && !stored.iter().any(|held| held.asset_id == asset.asset_id)
        {
            stored.push(asset.clone());
        }
        let answers = assets::resolve_assets(&stored);
        let mut book: HashMap<String, Resolved> = HashMap::new();
        for entry in entries {
            let answer = if entry.badge {
                Resolved::Badge
            } else {
                answers
                    .get(&entry.image.asset_id)
                    .cloned()
                    .unwrap_or(Resolved::Pending)
            };
            book.insert(entry.image.asset_id.clone(), answer);
        }
        Self { answers: book }
    }
}

impl AssetResolver for PreviewResolver {
    fn resolve(&self, asset_id: &str) -> Resolved {
        self.answers
            .get(asset_id)
            .cloned()
            .unwrap_or(Resolved::Pending)
    }
}

/// What the reader's links report.
pub(crate) type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The painter's link ids: one counter per rendered README, threaded
/// through the render walk, so no two clickable elements ever share an
/// id (the same address twice is two links, and each click must land
/// on its own).
struct Links {
    next: usize,
}

impl Links {
    fn fresh(&mut self, url: &str) -> SharedString {
        self.next += 1;
        SharedString::from(format!("link-{}-{url}", self.next))
    }
}

/// The blocks as elements, in the tab's layout.
pub(crate) fn render_blocks(blocks: &[Block], on_link: &LinkHandler) -> Vec<AnyElement> {
    render_blocks_at(blocks, on_link, &ReadmeLayout::tab())
}

/// The blocks as elements, in the row's layout: the same README, at
/// the row's sizes.
pub(crate) fn render_preview_blocks(blocks: &[Block], on_link: &LinkHandler) -> Vec<AnyElement> {
    render_blocks_at(blocks, on_link, &ReadmeLayout::preview())
}

/// The blocks as elements, at one layout.
fn render_blocks_at(
    blocks: &[Block],
    on_link: &LinkHandler,
    layout: &ReadmeLayout,
) -> Vec<AnyElement> {
    let mut links = Links { next: 0 };
    blocks
        .iter()
        .map(|block| render_block(block, on_link, &mut links, layout))
        .collect()
}

/// One block as an element.
fn render_block(
    block: &Block,
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    match block {
        Block::Heading { level, inlines } => div()
            .text_size(heading_size(*level, layout))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text())
            .mt(layout.gap)
            .flow(inlines, on_link, links, layout)
            .into_any_element(),
        Block::Paragraph(inlines) => div()
            .text_size(layout.body)
            .text_color(theme::text())
            .flow(inlines, on_link, links, layout)
            .into_any_element(),
        Block::List { ordered, items } => render_list(*ordered, items, on_link, links, layout),
        Block::Code { language, code } => render_code(language.as_deref(), code, layout),
        Block::Table { header, rows } => render_table(header, rows, on_link, links, layout),
        Block::Quote(inner) => div()
            .flex()
            .flex_col()
            .gap_2()
            .pl_3()
            .border_l_2()
            .border_color(theme::border())
            .children(
                inner
                    .iter()
                    .map(|block| render_block(block, on_link, links, layout)),
            )
            .into_any_element(),
        Block::Rule => div()
            .h(px(1.0))
            .my_2()
            .bg(theme::border())
            .into_any_element(),
    }
}

/// The size of a heading of the given level.
fn heading_size(level: u8, layout: &ReadmeLayout) -> gpui::Pixels {
    match level {
        1 => layout.h1,
        2 => layout.h2,
        3 => layout.h3,
        _ => layout.h4,
    }
}

/// The list: its items, one per line — the marker, the flowed text,
/// and any child blocks (a nested list, most commonly) indented under
/// it.
fn render_list(
    ordered: bool,
    items: &[ListItem],
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    let rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(index, item)| render_list_item(ordered, index, item, on_link, links, layout))
        .collect();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(rows)
        .into_any_element()
}

/// One list item: its marker (a bullet, a number, or a task box), its
/// flowed text, and its child blocks.
fn render_list_item(
    ordered: bool,
    index: usize,
    item: &ListItem,
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    let marker = match (ordered, item.task) {
        (_, Some(false)) => "☐".to_string(),
        (_, Some(true)) => "☑".to_string(),
        (true, None) => format!("{}.", index + 1),
        (false, None) => "·".to_string(),
    };
    let mut row = div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .child(
            div()
                .text_size(layout.body)
                .text_color(theme::text_muted())
                .child(SharedString::from(marker)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(layout.body)
                .text_color(theme::text())
                .flow(&item.inlines, on_link, links, layout),
        );
    if !item.children.is_empty() {
        row = row.child(
            div().flex().flex_col().pl_4().children(
                item.children
                    .iter()
                    .map(|block| render_block(block, on_link, links, layout))
                    .collect::<Vec<_>>(),
            ),
        );
    }
    row.into_any_element()
}

/// The code block: the language's name and the code itself.
fn render_code(language: Option<&str>, code: &str, layout: &ReadmeLayout) -> AnyElement {
    let mut column = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_sm()
        .bg(theme::surface_alt());
    if let Some(language) = language {
        column = column.child(
            div()
                .text_size(layout.code)
                .text_color(theme::text_muted())
                .child(SharedString::from(language.to_string())),
        );
    }
    column = column.child(
        div()
            .font_family(theme::MONO_FONT)
            .text_size(layout.code)
            .text_color(theme::text())
            .child(SharedString::from(code.to_string())),
    );
    column.into_any_element()
}

/// The table: its header row and its body rows. A cell is a run of
/// inlines, so a link inside a cell is a link.
fn render_table(
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    let head = div()
        .flex()
        .flex_row()
        .gap_3()
        .pb_1()
        .border_b_1()
        .border_color(theme::border())
        .children(header.iter().map(|cell| {
            div()
                .flex_1()
                .min_w_0()
                .text_size(layout.body)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .flow(cell, on_link, links, layout)
        }));
    let body: Vec<AnyElement> = rows
        .iter()
        .map(|row| {
            div()
                .flex()
                .flex_row()
                .gap_3()
                .py_1()
                .children(row.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(layout.body)
                        .text_color(theme::text_muted())
                        .flow(cell, on_link, links, layout)
                }))
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .mb_2()
        .child(head)
        .children(body)
        .into_any_element()
}

/// The flowing form of an inline run: adjacent text runs merged into
/// one, so an unparseable construct (which the parser hands back as
/// single-character text runs) still wraps as words inside one child.
fn merge_text_runs(inlines: &[Inline]) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        let carried = match (merged.last_mut(), inline) {
            (Some(Inline::Text(prior)), Inline::Text(text)) => {
                prior.push_str(text);
                true
            }
            _ => false,
        };
        if !carried {
            merged.push(inline.clone());
        }
    }
    merged
}

/// The flow trait: one container's inline run as a wrapping,
/// baseline-aligned row of flowed children. A run with no styling in
/// it is one text child — plain paragraphs wrap as well as gpui wraps
/// plain text, with no per-run seams at all.
trait Flow {
    fn flow(
        self,
        inlines: &[Inline],
        on_link: &LinkHandler,
        links: &mut Links,
        layout: &ReadmeLayout,
    ) -> Self;
}

impl Flow for gpui::Div {
    fn flow(
        self,
        inlines: &[Inline],
        on_link: &LinkHandler,
        links: &mut Links,
        layout: &ReadmeLayout,
    ) -> Self {
        let merged = merge_text_runs(inlines);
        let plain = merged
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)));
        if plain {
            let text = merged
                .iter()
                .map(|inline| match inline {
                    Inline::Text(text) => text.as_str(),
                    _ => "",
                })
                .collect::<Vec<_>>()
                .join("");
            return self.child(SharedString::from(text));
        }
        self.flex()
            .flex_row()
            .flex_wrap()
            .items_baseline()
            .children(
                merged
                    .iter()
                    .map(|inline| render_inline(inline, on_link, links, layout)),
            )
    }
}

/// One inline as a flowed child.
fn render_inline(
    inline: &Inline,
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    match inline {
        Inline::Text(text) => div()
            .child(SharedString::from(text.clone()))
            .into_any_element(),
        Inline::Code(code) => div()
            .font_family(theme::MONO_FONT)
            .text_size(layout.code)
            .text_color(theme::accent())
            .child(SharedString::from(code.clone()))
            .into_any_element(),
        Inline::Emphasis(inner) => div()
            .italic()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_baseline()
            .children(
                merge_text_runs(inner)
                    .iter()
                    .map(|inline| render_inline(inline, on_link, links, layout)),
            )
            .into_any_element(),
        Inline::Strong(inner) => div()
            .font_weight(FontWeight::BOLD)
            .flex()
            .flex_row()
            .flex_wrap()
            .items_baseline()
            .children(
                merge_text_runs(inner)
                    .iter()
                    .map(|inline| render_inline(inline, on_link, links, layout)),
            )
            .into_any_element(),
        Inline::Link { text, url } => render_link(text, url, on_link, links, layout),
        Inline::Image { alt, asset } => render_image(alt, asset, layout),
    }
}

/// The link: its text, clickable — it leaves the app through the
/// platform's opener and nowhere else.
fn render_link(
    text: &[Inline],
    url: &str,
    on_link: &LinkHandler,
    links: &mut Links,
    layout: &ReadmeLayout,
) -> AnyElement {
    let handler = Rc::clone(on_link);
    let target = url.to_string();
    let id = links.fresh(url);
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_wrap()
        .items_baseline()
        .text_color(theme::accent())
        .cursor_pointer()
        .hover(|style| style.text_color(theme::text()))
        .on_click(move |_, window, cx| handler(&target, window, cx))
        .children(
            merge_text_runs(text)
                .iter()
                .map(|inline| render_inline(inline, on_link, links, layout)),
        )
        .into_any_element()
}

/// The image: the cached file scaled down inside its box (never
/// cropped), the placeholder, the quiet marker, or the badge's own
/// quiet marker — a badge is a picture the reader never shows.
fn render_image(alt: &str, asset: &Resolved, layout: &ReadmeLayout) -> AnyElement {
    match asset {
        Resolved::File(path) => img(path.clone())
            .max_w(layout.image_width)
            .max_h(layout.image_height)
            .object_fit(ObjectFit::ScaleDown)
            .into_any_element(),
        Resolved::Pending => {
            let label = if alt.is_empty() {
                "the image is on its way…".to_string()
            } else {
                format!("the image is on its way… ({alt})")
            };
            div()
                .h(px(48.0))
                .text_size(layout.code)
                .text_color(theme::text_muted())
                .child(SharedString::from(label))
                .into_any_element()
        }
        Resolved::Missing => div()
            .h(px(28.0))
            .text_size(layout.code)
            .text_color(theme::text_muted())
            .child("the image is not there")
            .into_any_element(),
        Resolved::Badge => div()
            .h(px(28.0))
            .text_size(layout.code)
            .text_color(theme::text_muted())
            .child("a badge")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_resolver_answers_the_three_states() {
        let mut answers = HashMap::new();
        answers.insert(
            "cached".to_string(),
            Resolved::File(PathBuf::from("/cache/cached.img")),
        );
        answers.insert("queued".to_string(), Resolved::Pending);
        answers.insert("gone".to_string(), Resolved::Missing);
        let resolver = TabResolver::new(&answers);
        assert_eq!(
            resolver.resolve("cached"),
            Resolved::File(PathBuf::from("/cache/cached.img"))
        );
        assert_eq!(resolver.resolve("queued"), Resolved::Pending);
        assert_eq!(resolver.resolve("gone"), Resolved::Missing);
        assert_eq!(
            resolver.resolve("unknown"),
            Resolved::Missing,
            "an id the store never knew is missing"
        );
    }

    #[test]
    fn adjacent_text_runs_merge_into_one_child() {
        // The parser hands back single-character text runs when a
        // construct does not parse; merged, they wrap as words.
        let runs = vec![
            Inline::Text("a".to_string()),
            Inline::Text(" b".to_string()),
            Inline::Code("x".to_string()),
            Inline::Text(" c".to_string()),
            Inline::Text("d".to_string()),
        ];
        let merged = merge_text_runs(&runs);
        assert_eq!(merged.len(), 3, "five runs, two seams: {merged:?}");
        assert_eq!(merged[0], Inline::Text("a b".to_string()));
        assert_eq!(merged[2], Inline::Text(" cd".to_string()));
    }
}
