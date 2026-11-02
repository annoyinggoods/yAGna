//! The shell chrome: the brand header, the two cards the shell opens,
//! the notification card, and the update's own slot.
//!
//! The shell is what surrounds the work: the product's mark and name
//! at the top, the menu beside them, the gear that opens the settings,
//! and — in the corner, over the body — the notifications. Nothing
//! here knows about the catalog; the shell paints what the pane hands
//! it.
//!
//! The two brand marks are the PNGs the built-in asset source serves:
//! the product's mark, drawn in the header and in the About card, and
//! the author's small mark, which says who built it. Neither is ever a
//! window icon, and the author's mark never sits in the tab strip.
//!
//! Items: `Notification` — what a card says; `Dialog` — the two cards
//! the shell opens; `render_brand` — the mark, the wordmark, the
//! version; `render_about`/`render_help` — the two cards;
//! `render_notification` — the notification card;
//! `render_update_card` — the update's own slot; `render_author_mark`
//! — the small mark.
//! Depends on: gpui (the elements), crate::ui::theme (the inks),
//! crate::ui::glyphs (the brand paths), crate::ui::icons (the shapes),
//! crate::ui::tabs (the icon element).
//! Called by: crate::ui (the list pane, the window's chrome).

use std::path::Path;
use std::rc::Rc;

use gpui::{
    App, ImageSource, ObjectFit, Pixels, Resource, SharedString, Window, div, img, prelude::*, px,
};

use super::glyphs;
use super::icons::{self, Icon};
use super::rows::RowLayout;
use super::tabs::icon_element;
use super::theme;
use super::typography::{self, TypeScale};

/// The version the build stamped.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The product mark's height in the header.
const MARK_HEIGHT: Pixels = px(22.0);
/// The product mark's height in the About card.
const ABOUT_MARK_HEIGHT: Pixels = px(56.0);
/// The author mark's height.
const AUTHOR_MARK_HEIGHT: Pixels = px(14.0);
/// The About card's width.
const ABOUT_WIDTH: Pixels = px(420.0);
/// The Help card's width.
const HELP_WIDTH: Pixels = px(460.0);
/// The notification card's width.
const NOTICE_WIDTH: Pixels = px(380.0);

/// The licence the app is under, named — never quoted. The ethical use
/// restrictions are the point of the licence, and the text itself
/// lives with the licence file.
const LICENCE_LINE: &str =
    "Licensed under the Hippocratic License — the ethical use restrictions are the point.";

/// What a notification card says: the verb, the body, and whether it
/// is an alarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    /// What happened, in a word or two: "Update finished".
    pub verb: String,
    /// The detail: the summary, the problem.
    pub body: String,
    /// Whether the card is an alarm rather than a fact.
    pub error: bool,
}

/// The cards the shell opens over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// Who this is, which version, and where the data lives.
    About,
    /// The keyboard's map.
    Help,
}

/// What a card's way-out calls: the card closes.
pub(crate) type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a notification's dismiss calls: the card's own index, so the
/// one that was clicked is the one that goes.
pub(crate) type DismissHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The brand header: the product's mark, the wordmark, and the
/// version.
pub(crate) fn render_brand() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(render_mark(MARK_HEIGHT))
        .child(
            div()
                .text_size(RowLayout::top_size())
                .text_color(theme::text())
                .child("yAGna"),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(VERSION.to_string())),
        )
}

/// The product's mark: the PNG the asset source serves, drawn at the
/// given height. The width follows the picture's own shape.
pub(crate) fn render_mark(height: Pixels) -> gpui::AnyElement {
    img(ImageSource::Resource(Resource::Embedded(
        glyphs::BRAND_MARK_ASSET.into(),
    )))
    .h(height)
    .object_fit(ObjectFit::Contain)
    .into_any_element()
}

/// The author's mark: the small picture and the words beside it.
pub(crate) fn render_author_mark() -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            img(ImageSource::Resource(Resource::Embedded(
                glyphs::BRAND_AUTHOR_ASSET.into(),
            )))
            .h(AUTHOR_MARK_HEIGHT)
            .object_fit(ObjectFit::Contain),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child("built by ag"),
        )
        .into_any_element()
}

/// The update's own slot: a bordered card with the sweep's status.
/// Empty when the update has nothing to say.
pub(crate) fn render_update_card(line: Option<&str>) -> gpui::Div {
    let Some(line) = line else {
        return div();
    };
    div()
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(theme::border())
        .text_size(RowLayout::small_size())
        .text_color(theme::status_info())
        .child(SharedString::from(line.to_string()))
}

/// The notification card: the icon, the verb, the body, and the way
/// out. It sits over the body, in the corner, and it stays until the
/// reader dismisses it.
pub(crate) fn render_notification(
    notification: &Notification,
    index: usize,
    tip: &super::tooltip::TooltipHandle,
    on_dismiss: &DismissHandler,
) -> gpui::Stateful<gpui::Div> {
    let icon = if notification.error {
        Icon::AlertTriangle
    } else {
        Icon::Check
    };
    let dismiss = Rc::clone(on_dismiss);
    let close = tip.clone().attach(
        div()
            .id(SharedString::from(format!("notification-dismiss-{index}")))
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .on_click(move |_, window, cx| dismiss(index, window, cx))
            .child(icon_element(Icon::Close, theme::notice_text())),
        "dismiss",
    );
    div()
        .id(SharedString::from(format!("notification-{index}")))
        .w(NOTICE_WIDTH)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::notice_bg())
        .border_1()
        .border_color(theme::border())
        .child(icon_element(icon, theme::notice_text()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::notice_text())
                        .child(SharedString::from(notification.verb.clone())),
                )
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::notice_text())
                        .child(SharedString::from(notification.body.clone())),
                ),
        )
        .child(close)
}

/// The About card: the mark, the wordmark, the version, the licence
/// the app is under, and where the data lives.
pub(crate) fn render_about(
    data_dir: Option<&Path>,
    on_close: &CloseHandler,
) -> gpui::Stateful<gpui::Div> {
    let close = Rc::clone(on_close);
    let directory = match data_dir {
        Some(path) => path.display().to_string(),
        None => "the data directory could not be found".to_string(),
    };
    modal(
        "about",
        "about yAGna",
        ABOUT_WIDTH,
        &close,
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .child(render_mark(ABOUT_MARK_HEIGHT))
            .child(
                div()
                    .text_size(typography::scaled(TypeScale::SURFACE_TITLE))
                    .text_color(theme::text())
                    .child("yAGna"),
            )
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(format!("version {VERSION}"))),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text())
                    .child(LICENCE_LINE),
            )
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(format!("data directory: {directory}"))),
            ),
    )
}

/// The Help card: the keyboard's map.
pub(crate) fn render_help(on_close: &CloseHandler) -> gpui::Stateful<gpui::Div> {
    let close = Rc::clone(on_close);
    let mut shortcuts = div().flex().flex_col().gap_1();
    for (keys, what) in SHORTCUTS {
        shortcuts = shortcuts.child(shortcut_row(keys, what));
    }
    modal(
        "help",
        "shortcuts",
        HELP_WIDTH,
        &close,
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(RowLayout::small_size())
                            .text_color(theme::text_muted())
                            .child("the keyboard"),
                    )
                    .child(shortcuts),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(RowLayout::small_size())
                            .text_color(theme::text_muted())
                            .child("the icons"),
                    )
                    .child(render_icon_legend()),
            ),
    )
}

/// The icon legend: every shape the app draws, with the name it is
/// announced with.
pub(crate) fn render_icon_legend() -> gpui::Div {
    let cells: Vec<gpui::AnyElement> = icons::all().iter().map(|icon| legend_cell(*icon)).collect();
    div().flex().flex_row().flex_wrap().gap_3().children(cells)
}

/// One legend cell: the shape, and the name beside it.
fn legend_cell(icon: Icon) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(icon_element(icon, theme::text_muted()))
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(icon.name().to_string())),
        )
        .into_any_element()
}

/// The keyboard's map: the keys, and what they do.
const SHORTCUTS: [(&str, &str); 8] = [
    ("/", "focus the search"),
    ("ctrl+,", "open the settings"),
    ("ctrl+tab", "cycle the tabs forward"),
    ("ctrl+shift+tab", "cycle the tabs backward"),
    ("ctrl+w", "close the tab"),
    ("ctrl+r", "back to the library"),
    ("enter", "open the focused row"),
    ("up / down", "walk the rows"),
];

/// One shortcut line: the keys on the left, the meaning on the right.
fn shortcut_row(keys: &str, what: &str) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .child(SharedString::from(keys.to_string())),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .child(SharedString::from(what.to_string())),
        )
}

/// The card's frame: the veil over the window, the card in the
/// middle, the title, and the way out.
fn modal(
    id: &'static str,
    title: &'static str,
    width: Pixels,
    on_close: &CloseHandler,
    body: gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    let close = Rc::clone(on_close);
    div()
        .id(SharedString::from(format!("{id}-dialog")))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::Hsla::from(theme::background()).opacity(0.75))
        .child(
            div()
                .id(SharedString::from(format!("{id}-card")))
                .w(width)
                .max_h(px(600.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(RowLayout::top_size())
                                .text_color(theme::text())
                                .child(title),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("{id}-close")))
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_alt()))
                                .text_size(RowLayout::small_size())
                                .text_color(theme::accent())
                                .on_click(move |_, window, cx| close(window, cx))
                                .child("close"),
                        ),
                )
                .child(body)
                .child(render_author_mark()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two brand marks are the paths the built-in asset source
    /// serves; the shell never names a path of its own.
    #[test]
    fn the_shell_draws_the_marks_the_asset_source_serves() {
        assert_eq!(glyphs::BRAND_MARK_ASSET, "yagna-brand/yagna.png");
        assert_eq!(glyphs::BRAND_AUTHOR_ASSET, "yagna-brand/ag-logo.png");
    }

    /// The licence is named, never quoted: the card carries the name
    /// and nothing of the text.
    #[test]
    fn the_licence_is_named_and_not_quoted() {
        assert!(LICENCE_LINE.contains("Hippocratic License"));
        assert!(!LICENCE_LINE.contains("Permission is hereby granted"));
    }

    /// The help card's map names the keys the window really answers to.
    #[test]
    fn the_help_card_names_the_keys_the_window_answers_to() {
        let keys: Vec<&str> = SHORTCUTS.iter().map(|(keys, _)| *keys).collect();
        for key in ["/", "ctrl+,", "ctrl+tab", "ctrl+w", "ctrl+r", "enter"] {
            assert!(keys.contains(&key), "the map names {key}");
        }
    }

    /// The version is the build's own.
    #[test]
    fn the_version_is_the_builds_own() {
        assert!(!VERSION.is_empty());
        assert!(VERSION.chars().all(|c| c.is_ascii_digit() || c == '.'));
    }
}
