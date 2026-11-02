//! The catalog row: the pitch's layout rendered from a `RepoRow`, the
//! chevron that opens it, the tag chips, and the expanded preview —
//! the README rendered, the badge count, and the preview image.
//!
//! The collapsed row keeps Part04's geometry exactly. The expanded row
//! is the same header with the preview block beneath it, so the row
//! grows instead of the layout jumping. The chevron is a shape that
//! rotates: the framework's animation runs it from one angle to the
//! other, and the row's generation says which way it runs.
//!
//! The preview is the README *rendered* — the same blocks the
//! repository tab paints, through the same renderer. The raw markdown
//! source never reaches the screen: a reader sees the words, not the
//! link syntax.
//!
//! Items: `RowDisplay` — the per-row view model; `RowLayout` — the
//! row's fixed metrics; `ExcerptLines` — the excerpt cut to two lines;
//! `Chevron` — the chevron's state; `RowInput` — what one row's
//! element needs; `RowActions` — what the row's interactive parts
//! report; `render_row` — the element.
//! Depends on: yagna_core (RepoRow), yagna_store (the preview's
//! records), crate::ui::theme (the palette), crate::ui::glyphs (the
//! chevron's shape), crate::ui::expand (the preview), crate::ui::readme
//! (the renderer).
//! Called by: crate::ui (the list's item renderer).

use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Font, FontStyle, FontWeight, ObjectFit,
    Pixels, SharedString, TextRun, Transformation, Window, div, img, percentage, prelude::*, px,
    svg,
};
use yagna_core::model::RepoRow;

use super::expand::PreviewView;
use super::glyphs::CHEVRON_ASSET;
use super::platform;
use super::readme;
use super::theme;
use super::typography::{self, TypeScale};

/// The row's fixed metrics: the collapsed row's height, its font sizes
/// and the content width the excerpt is shaped against.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowLayout;

impl RowLayout {
    /// The height of the collapsed row.
    pub(crate) const HEIGHT: Pixels = px(104.0);
    /// The name and the right-aligned metrics: the row's title.
    pub(crate) fn top_size() -> Pixels {
        typography::scaled(TypeScale::TITLE)
    }
    /// The meta line, the tags and the excerpt: the body size.
    pub(crate) fn small_size() -> Pixels {
        typography::scaled(TypeScale::BODY)
    }
    /// A section heading, one step above the body.
    pub(crate) fn heading_size() -> Pixels {
        typography::scaled(TypeScale::HEADING)
    }
    /// The chevron column at the left edge.
    pub(crate) const CHEVRON_WIDTH: Pixels = px(20.0);
    /// The row's horizontal padding (px_3 on both sides).
    pub(crate) const H_PADDING: Pixels = px(24.0);
    /// Slack kept between the computed content width and the real one so
    /// the shaped excerpt never overflows its box.
    pub(crate) const WIDTH_SLACK: Pixels = px(8.0);
    /// The chevron's own size.
    pub(crate) const CHEVRON_SIZE: Pixels = px(12.0);
    /// The preview image's height.
    pub(crate) const PREVIEW_IMAGE_HEIGHT: Pixels = px(220.0);

    /// The width the excerpt is shaped against, given the viewport
    /// width: the row's box minus padding, the chevron column and slack.
    pub(crate) fn content_width(viewport_width: Pixels) -> Pixels {
        (viewport_width - Self::H_PADDING - Self::CHEVRON_WIDTH - Self::WIDTH_SLACK).max(px(120.0))
    }
}

/// The chevron's state: whether the row is open, and how many times it
/// was toggled — the generation is what makes the rotation run again.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Chevron {
    pub open: bool,
    pub generation: u32,
    pub rowid: i64,
}

/// A row's click handler: the event, the window, the app.
pub(crate) type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A chip's click handler: the tag's name rides along.
pub(crate) type TagHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the row's interactive parts report.
#[derive(Clone)]
pub(crate) struct RowActions {
    /// The chevron was clicked.
    pub on_chevron: ClickHandler,
    /// A tag chip was clicked; the tag's name rides along.
    pub on_remove_tag: TagHandler,
    /// The add-tag chip was clicked: the sidebar opens the editor.
    pub on_edit: ClickHandler,
    /// The repository's name was clicked: its tab opens.
    pub on_open: ClickHandler,
}

/// The per-row view model: every string the row paints, built once per
/// page (the excerpt is shaped once per row per width — the scroll path
/// reuses it, never re-shapes per frame).
#[derive(Debug, Clone)]
pub(crate) struct RowDisplay {
    /// The repository name in capitals — line 1's left side.
    pub(crate) name: String,
    /// The primary language, lowercase — line 1's right side.
    pub(crate) language: String,
    /// The star count — line 1's right side.
    pub(crate) stars: String,
    /// The health score as `n/12` — line 1's right side; the breakdown
    /// view is Part06's.
    pub(crate) health: String,
    /// The fit score beside the health — its name and one decimal, or
    /// the em dash for a row no spec has scored yet.
    pub(crate) fit: String,
    /// Line 2: license name, topic tags, age, last-commit date.
    pub(crate) meta: String,
    /// The user's tags, as chips — empty is empty, no placeholder.
    pub(crate) tags: Vec<String>,
    /// Lines 5–6: the README excerpt cut to width.
    pub(crate) excerpt: ExcerptLines,
}

/// The excerpt cut to at most two lines: the second ends with an
/// ellipsis when the text did not fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExcerptLines {
    pub(crate) first: String,
    pub(crate) second: String,
}

impl ExcerptLines {
    /// No excerpt stored (no README fetched yet) renders as nothing.
    pub(crate) fn empty() -> Self {
        Self {
            first: String::new(),
            second: String::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.first.is_empty() && self.second.is_empty()
    }
}

/// Builds the row's static strings (everything except the width-dependent
/// excerpt shaping). The numbers carry their names: a bare "51" and a
/// bare "8/12" are just numbers.
pub(crate) fn build_display(row: &RepoRow, now: DateTime<Utc>) -> RowDisplay {
    RowDisplay {
        name: row.repo_ref.name.to_uppercase(),
        language: row
            .primary_language
            .as_deref()
            .unwrap_or("unknown")
            .to_lowercase(),
        stars: format!("★ {}", row.stars),
        health: format!("health {}/12", row.health.total),
        fit: match row.fit_score {
            Some(total) => format!("fit {total:.1}"),
            None => "fit —".to_string(),
        },
        meta: meta_line(row, now),
        tags: row.tags.clone(),
        excerpt: ExcerptLines::empty(),
    }
}

/// Line 2's run: license name, the topic tags, the age and the
/// last-commit date (UTC — local rendering is a later concern). Missing
/// pieces are skipped, never rendered as dashes. The topics carry their
/// name: without it they are just words between two dates.
fn meta_line(row: &RepoRow, now: DateTime<Utc>) -> String {
    let mut pieces: Vec<String> = Vec::new();
    if let Some(license) = row.license_name.as_deref() {
        pieces.push(license.to_string());
    }
    if !row.topics.is_empty() {
        pieces.push(format!("topics: {}", row.topics.join(", ")));
    }
    pieces.push(age_label(row.created_at, now));
    pieces.push(row.last_commit_at.format("%Y-%m-%d").to_string());
    pieces.join("  ·  ")
}

/// The repository's age in `3y` / `8m` / `12d` form, measured against
/// the session instant (the pure core never reads a clock): whole
/// calendar months, adjusted down when the day-of-month has not been
/// reached yet.
fn age_label(created_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    use chrono::Datelike;
    let (later, earlier) = (now.naive_utc(), created_at.naive_utc());
    let mut months = (later.year() as i64 - earlier.year() as i64) * 12
        + (later.month() as i64 - earlier.month() as i64);
    if later.day() < earlier.day() {
        months -= 1;
    }
    if months >= 12 {
        format!("{}y", months / 12)
    } else if months >= 1 {
        format!("{}m", months)
    } else {
        format!("{}d", (now - created_at).num_days().max(0))
    }
}

/// The `Font` the excerpt is shaped and painted with: the UI family in
/// italic at the small size — the same on both sides so the measurement
/// cannot drift from the paint.
pub(crate) fn excerpt_font() -> Font {
    Font {
        family: SharedString::from(theme::UI_FONT),
        features: gpui::FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Italic,
    }
}

/// Shapes the excerpt to `width`: two lines at most, the second ending
/// in an ellipsis when the text did not fit. Uses binary search over the
/// text's character boundaries against measured line widths — glyph
/// positions by measurement, never character counts.
pub(crate) fn shape_excerpt(text: &str, width: Pixels, window: &Window) -> ExcerptLines {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return ExcerptLines::empty();
    }
    let text_system = window.text_system();
    let font = excerpt_font();
    let size = RowLayout::small_size();
    if shape_width(text_system, &flat, &font, size) <= width {
        return ExcerptLines {
            first: flat,
            second: String::new(),
        };
    }
    let first_end = largest_fitting_prefix(text_system, &flat, width, &font, size);
    let rest = flat[first_end..].trim_start();
    if rest.is_empty() {
        return ExcerptLines {
            first: flat,
            second: String::new(),
        };
    }
    if shape_width(text_system, rest, &font, size) <= width {
        return ExcerptLines {
            first: flat[..first_end].trim_end().to_string(),
            second: rest.to_string(),
        };
    }
    let ellipsis_width = shape_width(text_system, "…", &font, size);
    let budget = (width - ellipsis_width).max(px(0.0));
    let cut = largest_fitting_prefix(text_system, rest, budget, &font, size);
    ExcerptLines {
        first: flat[..first_end].trim_end().to_string(),
        second: format!("{}…", rest[..cut].trim_end()),
    }
}

/// The shaped width of one piece of text. Every shaped string gets its
/// own run, whose length is that string's own byte length: a run that
/// reaches past the end of the text it describes makes the text backend
/// read out of bounds.
pub(crate) fn shape_width(
    text_system: &gpui::WindowTextSystem,
    text: &str,
    font: &Font,
    size: Pixels,
) -> Pixels {
    let run = run_for(text, font);
    text_system
        .shape_line(text.to_string().into(), size, &[run], None)
        .width
}

/// The text run for one piece of text: its length is that text's own
/// byte length, never a longer text's.
fn run_for(text: &str, font: &Font) -> TextRun {
    TextRun {
        len: text.len(),
        font: font.clone(),
        color: theme::text_muted().into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// The byte offset of the longest prefix of `text` whose shaped width
/// fits `max_width` — a binary search over character boundaries.
fn largest_fitting_prefix(
    text_system: &gpui::WindowTextSystem,
    text: &str,
    max_width: Pixels,
    font: &Font,
    size: Pixels,
) -> usize {
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut low = 0usize;
    let mut high = boundaries.len() - 1;
    while low < high {
        let mid = (low + high).div_ceil(2);
        let prefix = &text[..boundaries[mid]];
        if shape_width(text_system, prefix, font, size) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    boundaries[low]
}

/// The row element: the chevron at the left edge, then the content
/// column — line 1, line 2, an empty line, the tag chips, and the two
/// italic excerpt lines. An open row carries the preview beneath it,
/// or the one line that says why the README it asked for is not there.
///
/// The row is a card: a repository is a thing of its own, and the eye
/// needs to see where one ends and the next begins.
pub(crate) fn render_row(input: &RowInput<'_>) -> AnyElement {
    let header = div()
        .flex()
        .flex_row()
        .min_h(RowLayout::HEIGHT)
        .px(RowLayout::H_PADDING / 2.0)
        .when(input.focused, |header| header.bg(theme::surface_alt()))
        .child(render_chevron(&input.chevron, input.actions))
        .child(render_column(input));
    let body = match input.preview {
        None if input.pending => div()
            .flex()
            .flex_col()
            .w_full()
            .child(header)
            .child(render_preview_placeholder()),
        None => div()
            .flex()
            .flex_col()
            .w_full()
            .child(header)
            .when_some(input.readme_note.as_deref(), |column, note| {
                column.child(render_readme_note(note))
            }),
        Some(preview) => div()
            .flex()
            .flex_col()
            .w_full()
            .child(header)
            .child(render_preview(preview)),
    };
    div()
        .flex()
        .flex_col()
        .w_full()
        .mx_2()
        .my(px(5.0))
        .py(px(6.0))
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .overflow_hidden()
        .child(body)
        .into_any_element()
}

/// The one line an open row shows when its README could not be
/// fetched: a fact in the muted ink, never an alarm.
fn render_readme_note(note: &str) -> gpui::Div {
    div()
        .px(RowLayout::H_PADDING)
        .py_2()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(format!(
            "the readme is missing — {note}"
        )))
}

/// Everything one row's element needs: the view model, the preview (and
/// whether it is still on its way), why the README is missing when it
/// is, the chevron's state, whether the row is focused, the row's own
/// id, and what the row's interactive parts report.
pub(crate) struct RowInput<'a> {
    pub display: &'a RowDisplay,
    pub preview: Option<&'a PreviewView>,
    pub pending: bool,
    /// Why the row's on-demand README fetch failed, when it did.
    pub readme_note: Option<String>,
    pub chevron: Chevron,
    pub focused: bool,
    pub rowid: i64,
    pub actions: &'a RowActions,
}

/// The row's content column.
fn render_column(input: &RowInput<'_>) -> gpui::Div {
    let display = input.display;
    let actions = input.actions;
    let mut column = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .text_size(RowLayout::top_size())
                .child(render_name(display, input))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .text_color(theme::text_muted())
                        .child(SharedString::from(display.language.clone()))
                        .child(SharedString::from(display.stars.clone()))
                        .child(SharedString::from(display.health.clone()))
                        .child(SharedString::from(display.fit.clone())),
                ),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(display.meta.clone())),
        )
        // The pitch's empty line: a spacer, not a styled text row.
        .child(div().h(px(2.0)));
    column = column.child(render_chips(&display.tags, input.rowid, actions));
    if !display.excerpt.is_empty() {
        column = column
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .italic()
                    .text_color(theme::text_muted())
                    .when(!display.excerpt.first.is_empty(), |line| {
                        line.child(SharedString::from(display.excerpt.first.clone()))
                    }),
            )
            .when(!display.excerpt.second.is_empty(), |block| {
                block.child(
                    div()
                        .text_size(RowLayout::small_size())
                        .italic()
                        .text_color(theme::text_muted())
                        .child(SharedString::from(display.excerpt.second.clone())),
                )
            });
    }
    column
}

/// The repository's name: the click that opens the repository's tab.
fn render_name(display: &RowDisplay, input: &RowInput<'_>) -> gpui::AnyElement {
    let on_open = Rc::clone(&input.actions.on_open);
    div()
        .id(SharedString::from(format!("row-name-{}", input.rowid)))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.text_color(theme::accent()))
        .on_click(move |event, window, cx| on_open(event, window, cx))
        .child(SharedString::from(display.name.clone()))
        .into_any_element()
}

/// The user's tags as chips: each chip removes its tag when clicked.
fn render_chips(tags: &[String], rowid: i64, actions: &RowActions) -> AnyElement {
    let chips: Vec<AnyElement> = tags
        .iter()
        .map(|name| {
            let on_remove = Rc::clone(&actions.on_remove_tag);
            let label = name.clone();
            let clicked = label.clone();
            div()
                .id(SharedString::from(format!("chip-{label}")))
                .px_1()
                .rounded_sm()
                .bg(theme::surface_alt())
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .on_click(move |_, window, cx| on_remove(&clicked, window, cx))
                .child(SharedString::from(label))
                .into_any_element()
        })
        .collect();
    let on_edit = Rc::clone(&actions.on_edit);
    let adder = div()
        .id(SharedString::from(format!("chip-add-{rowid}")))
        .px_1()
        .rounded_sm()
        .bg(theme::surface_alt())
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |event, window, cx| on_edit(event, window, cx))
        .child("+")
        .into_any_element();
    div()
        .flex()
        .flex_row()
        .gap_1()
        .children(chips)
        .child(adder)
        .into_any_element()
}

/// How long the chevron takes to turn.
const CHEVRON_DURATION: Duration = Duration::from_millis(160);

/// The chevron's rotation, in turns: closed points down, open points
/// right. The turn is three quarters of a circle clockwise — the same
/// quarter turn the other way — because a fraction outside zero to one
/// is not a percentage. The generation says which way the rotation
/// runs: an odd generation opened last, an even one closed last.
fn chevron_angles(chevron: &Chevron) -> (f32, f32) {
    const OPEN: f32 = 0.75;
    if chevron.generation % 2 == 1 {
        (0.0, OPEN)
    } else {
        (OPEN, 0.0)
    }
}

/// The chevron: a shape that rotates as the row opens. A row that was
/// never toggled shows the shape at rest — no animation runs.
fn render_chevron(chevron: &Chevron, actions: &RowActions) -> AnyElement {
    let on_chevron = Rc::clone(&actions.on_chevron);
    let ink = if chevron.open {
        theme::accent()
    } else {
        theme::text_muted()
    };
    let shape = svg()
        .path(CHEVRON_ASSET)
        .size(RowLayout::CHEVRON_SIZE)
        .text_color(ink);
    let rotated: AnyElement = if chevron.generation == 0 {
        shape
            .with_transformation(Transformation::rotate(percentage(0.0)))
            .into_any_element()
    } else {
        let (from, to) = chevron_angles(chevron);
        let id = SharedString::from(format!("chevron-{}-{}", chevron.rowid, chevron.generation));
        shape
            .with_transformation(Transformation::rotate(percentage(from)))
            .with_animation(id, Animation::new(CHEVRON_DURATION), move |shape, delta| {
                let angle = from + (to - from) * delta;
                shape.with_transformation(Transformation::rotate(percentage(angle)))
            })
            .into_any_element()
    };
    div()
        .id(SharedString::from(format!("chevron-{}", chevron.rowid)))
        .w(RowLayout::CHEVRON_WIDTH)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(move |event, window, cx| on_chevron(event, window, cx))
        .child(rotated)
        .into_any_element()
}

/// The expanded row's preview: the README rendered, the badge count
/// that stands in for the badge wall, and the row's own image.
/// The image is either the cached file or a placeholder — never an
/// error state: an image that could not be fetched is simply absent.
fn render_preview(preview: &PreviewView) -> AnyElement {
    let mut column = div()
        .flex()
        .flex_col()
        .gap_2()
        .px(RowLayout::H_PADDING / 2.0)
        .pb_3()
        .pl(RowLayout::CHEVRON_WIDTH);
    if preview.badges > 0 {
        column = column.child(render_badge_line(preview.badges));
    }
    column = column.child(render_preview_readme(preview));
    if let Some(asset) = &preview.asset {
        column = column.child(render_preview_image(asset));
    }
    column.into_any_element()
}

/// The README, rendered: the blocks the core parsed, painted at the
/// row's own sizes. The box is bounded and clips — a preview is a
/// teaser, and a long README must not push the catalog off the
/// screen.
fn render_preview_readme(preview: &PreviewView) -> AnyElement {
    let handler = preview_link_handler();
    let elements = readme::render_preview_blocks(&preview.blocks, &handler);
    let mut column = div()
        .flex()
        .flex_col()
        .gap_2()
        .max_h(px(320.0))
        .overflow_hidden();
    if !elements.is_empty() {
        column = column.children(elements);
    }
    if preview.truncated {
        column = column.child(render_preview_note(preview.total_lines));
    }
    column.into_any_element()
}

/// The badge count: how many badges the README carried. A wall of CI
/// badges says nothing the reader can act on, so the count says what
/// the wall held.
fn render_badge_line(count: usize) -> AnyElement {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(badge_line(count)))
        .into_any_element()
}

/// The count in the reader's words: one badge is one badge.
fn badge_line(count: usize) -> String {
    if count == 1 {
        "1 badge".to_string()
    } else {
        format!("{count} badges")
    }
}

/// The muted line under the preview: how long the README was.
fn render_preview_note(total_lines: usize) -> AnyElement {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(format!("… ({total_lines} lines total)")))
        .into_any_element()
}

/// The preview's links: they leave the app through the platform's
/// opener, the same way the tab's links do. A link the platform
/// refuses to open is silent here — the row carries no status line of
/// its own.
fn preview_link_handler() -> readme::LinkHandler {
    Rc::new(|url: &str, _window: &mut Window, _cx: &mut App| {
        let _ = platform::open_external(url);
    })
}

/// The placeholder an open row shows while its preview is on its way.
fn render_preview_placeholder() -> AnyElement {
    div()
        .px(RowLayout::H_PADDING / 2.0)
        .pb_3()
        .pl(RowLayout::CHEVRON_WIDTH)
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child("the preview is on its way…")
        .into_any_element()
}

/// The preview image: the cached file, or a placeholder while it is on
/// its way.
fn render_preview_image(asset: &yagna_store::AssetRecord) -> AnyElement {
    match (&asset.path, asset.state.as_str()) {
        (Some(path), "present") => div()
            .h(RowLayout::PREVIEW_IMAGE_HEIGHT)
            .child(
                img(path.clone())
                    .h(RowLayout::PREVIEW_IMAGE_HEIGHT)
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element(),
        _ => div()
            .h(px(48.0))
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .child("the preview image is on its way…")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use yagna_core::health::HealthReport;
    use yagna_core::model::{Forge, RepoRef};

    fn instant(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    fn bare_row() -> RepoRow {
        RepoRow {
            repo_rowid: 1,
            repo_ref: RepoRef {
                forge: Forge::GitHub,
                owner: "someone".to_string(),
                name: "something".to_string(),
            },
            description: None,
            stars: 42,
            primary_language: Some("Rust".to_string()),
            license_name: Some("MIT License".to_string()),
            topics: vec!["cli".to_string()],
            created_at: instant(2023, 4, 1),
            starred_at: Some(instant(2026, 10, 1)),
            last_commit_at: instant(2026, 9, 30),
            first_commit_date: Some(instant(2023, 4, 1)),
            health: HealthReport::default(),
            fit_score: None,
            tags: vec!["reading list".to_string(), "cli".to_string()],
            excerpt: String::new(),
        }
    }

    #[test]
    fn build_display_renders_the_pitchs_strings() {
        let now = instant(2026, 10, 27);
        let display = build_display(&bare_row(), now);
        assert_eq!(display.name, "SOMETHING", "the name renders in capitals");
        assert_eq!(display.language, "rust", "the language renders lowercase");
        assert_eq!(display.stars, "★ 42", "the stars carry their glyph");
        assert_eq!(
            display.health, "health 0/12",
            "the health carries its name: a bare number says nothing"
        );
        assert!(display.meta.contains("MIT License"));
        assert!(
            display.meta.contains("topics: cli"),
            "the topics carry their name: {}",
            display.meta
        );
        assert!(display.meta.ends_with("2026-09-30"));
        assert_eq!(display.tags, ["reading list", "cli"]);
    }

    #[test]
    fn age_label_uses_years_months_then_days() {
        let now = instant(2026, 10, 27);
        assert_eq!(age_label(instant(2023, 4, 1), now), "3y");
        assert_eq!(age_label(instant(2026, 2, 27), now), "8m");
        assert_eq!(age_label(instant(2026, 10, 26), now), "1d");
    }

    #[test]
    fn the_fit_score_sits_beside_the_health() {
        let now = instant(2026, 10, 27);
        let mut row = bare_row();
        row.fit_score = Some(4.2);
        let display = build_display(&row, now);
        assert_eq!(display.fit, "fit 4.2", "the score carries its name");
        let unscored = build_display(&bare_row(), now);
        assert_eq!(
            unscored.fit, "fit —",
            "an unscored row says so — a zero would be a lie"
        );
    }

    #[test]
    fn meta_line_skips_missing_pieces() {
        let now = instant(2026, 10, 27);
        let mut row = bare_row();
        row.license_name = None;
        row.topics.clear();
        let display = build_display(&row, now);
        assert!(
            !display.meta.contains("·  ·"),
            "missing pieces are dropped, not padded: {:?}",
            display.meta
        );
    }

    #[test]
    fn content_width_stays_positive_for_narrow_windows() {
        assert_eq!(
            RowLayout::content_width(px(0.0)),
            px(120.0),
            "the floor keeps shaping sane on degenerate widths"
        );
    }

    #[test]
    fn excerpt_lines_report_their_own_emptiness() {
        let empty = ExcerptLines::empty();
        assert!(empty.is_empty());
        let lines = ExcerptLines {
            first: "one".to_string(),
            second: String::new(),
        };
        assert!(!lines.is_empty());
    }

    #[test]
    fn a_text_run_measures_its_own_text_and_no_more() {
        // The bug this pins: a run that describes more bytes than the
        // text it is shaped with makes the text backend read past the
        // end of the string — a crash in the shape, not a wrong width.
        let font = excerpt_font();
        assert_eq!(run_for("abc", &font).len, 3);
        assert_eq!(run_for("", &font).len, 0);
        assert_eq!(run_for("é", &font).len, 2, "the length is in bytes");
        assert_eq!(
            run_for("a longer text", &font).len,
            "a longer text".len(),
            "the run never reaches past its own text"
        );
    }

    #[test]
    fn the_chevron_rotates_towards_the_open_angle() {
        let opening = Chevron {
            open: true,
            generation: 1,
            rowid: 1,
        };
        let (from, to) = chevron_angles(&opening);
        assert_eq!(from, 0.0, "the chevron starts closed");
        assert_eq!(to, 0.75, "and turns three quarters of a circle");
        let closing = Chevron {
            open: false,
            generation: 2,
            rowid: 1,
        };
        let (from, to) = chevron_angles(&closing);
        assert_eq!(from, 0.75, "the chevron starts open");
        assert_eq!(to, 0.0, "and turns back");
        for (from, to) in [chevron_angles(&opening), chevron_angles(&closing)] {
            assert!((0.0..=1.0).contains(&from), "a turn is a percentage");
            assert!((0.0..=1.0).contains(&to), "a turn is a percentage");
        }
    }

    /// The badge count in the reader's words: one badge is one badge,
    /// a wall of them is counted.
    #[test]
    fn the_badge_count_reads_as_words() {
        assert_eq!(badge_line(1), "1 badge");
        assert_eq!(badge_line(12), "12 badges");
        assert_eq!(badge_line(0), "0 badges");
    }
}
