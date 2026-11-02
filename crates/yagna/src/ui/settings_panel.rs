//! The settings surface: a modal over the list, with a section per
//! part of the settings model.
//!
//! Every control commits on its own action — a click, or enter in a
//! field — never per keystroke. The theme picker's chips are the four
//! embedded themes' names; picking applies live and persists. The two
//! numeric settings (the cache cap, the page size) are the pane's text
//! fields, committed on enter — and so is the folder the local
//! repositories are discovered in.
//!
//! Items: `SettingsAction` — what the modal's controls report;
//! `SettingsActions` — the closures the pane hands in; `render` — the
//! modal.
//! Depends on: gpui (the elements, the key events), crate::settings
//! (the model), crate::ui::theme (the picker's names, the colors),
//! crate::ui::field (the two numeric fields).
//! Called by: crate::ui (the list pane, when the modal is open).

use std::rc::Rc;

use gpui::{KeyDownEvent, SharedString, Window, div, prelude::*, px};

use crate::settings::Settings;

use super::control;
use super::field::TextField;
use super::icons;
use super::rows::RowLayout;
use super::tabs;
use super::theme;

/// What the modal's controls report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    /// A theme chip was clicked: the theme applies live and persists.
    PickTheme(String),
    /// The sort-default button was clicked: the next key takes over.
    CycleSortDefault,
    /// The image-policy button was clicked: the next policy takes over.
    CycleImagePolicy,
    /// A toggle was clicked: the setting flips.
    ToggleAutoUpdate,
    ToggleExactFirstCommit,
    TogglePersistExpansion,
}

/// What closing the modal means.
pub(crate) type CloseHandler = Rc<dyn Fn(&mut Window, &mut gpui::App)>;
/// What one of the modal's buttons means.
pub(crate) type ActionHandler = Rc<dyn Fn(SettingsAction, &mut Window, &mut gpui::App)>;
/// What a keystroke in one of the modal's fields means.
pub(crate) type KeyHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut gpui::App)>;
/// What a field's click means: the focus goes to the field.
pub(crate) type ClickHandler = Rc<dyn Fn(&mut Window, &mut gpui::App)>;

/// The closures the pane hands the modal: what its controls do.
pub(crate) struct SettingsActions {
    pub on_close: CloseHandler,
    pub on_action: ActionHandler,
    pub on_cache_key: KeyHandler,
    pub on_cache_click: ClickHandler,
    pub on_page_key: KeyHandler,
    pub on_page_click: ClickHandler,
    pub on_local_key: KeyHandler,
    pub on_local_click: ClickHandler,
}

/// The modal: the dim sheet over everything, the panel in the middle.
pub(crate) fn render(
    draft: &Settings,
    cache_field: &TextField,
    page_field: &TextField,
    local_field: &TextField,
    actions: &SettingsActions,
    window: &mut Window,
) -> gpui::Stateful<gpui::Div> {
    let on_close = Rc::clone(&actions.on_close);
    div()
        .id("settings-modal")
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
                .id("settings-panel")
                .w(px(430.0))
                .max_h(gpui::px(600.0))
                .overflow_y_scroll()
                .scrollbar_width(super::repo::TabLayout::SCROLLBAR)
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(header(&on_close))
                .child(theme_section(draft, actions))
                .child(
                    section(
                        "opening",
                        row(
                            "sort repositories by",
                            cycle_button(
                                draft.sort_default.label(),
                                SettingsAction::CycleSortDefault,
                                actions,
                            ),
                        ),
                    )
                    .child(row(
                        "rows per page",
                        page_field.render(
                            window,
                            {
                                let on_key = Rc::clone(&actions.on_page_key);
                                move |event, window, cx| on_key(event, window, cx)
                            },
                            {
                                let on_click = Rc::clone(&actions.on_page_click);
                                move |window, cx| on_click(window, cx)
                            },
                        ),
                    )),
                )
                .child(
                    section(
                        "the sweep",
                        row(
                            "update on launch",
                            toggle(
                                draft.auto_update_on_launch,
                                SettingsAction::ToggleAutoUpdate,
                                actions,
                            ),
                        ),
                    )
                    .child(row(
                        "images",
                        cycle_button(
                            draft.image_policy.label(),
                            SettingsAction::CycleImagePolicy,
                            actions,
                        ),
                    )),
                )
                .child(section(
                    "the local repositories",
                    row(
                        "folder of clones",
                        local_field.render(
                            window,
                            {
                                let on_key = Rc::clone(&actions.on_local_key);
                                move |event, window, cx| on_key(event, window, cx)
                            },
                            {
                                let on_click = Rc::clone(&actions.on_local_click);
                                move |window, cx| on_click(window, cx)
                            },
                        ),
                    ),
                ))
                .child(
                    section(
                        "the catalog",
                        row(
                            "ask for the exact first commit",
                            toggle(
                                draft.exact_first_commit,
                                SettingsAction::ToggleExactFirstCommit,
                                actions,
                            ),
                        ),
                    )
                    .child(row(
                        "remember open rows across restarts",
                        toggle(
                            draft.persist_expansion,
                            SettingsAction::TogglePersistExpansion,
                            actions,
                        ),
                    ))
                    .child(row(
                        "cache cap (megabytes)",
                        cache_field.render(
                            window,
                            {
                                let on_key = Rc::clone(&actions.on_cache_key);
                                move |event, window, cx| on_key(event, window, cx)
                            },
                            {
                                let on_click = Rc::clone(&actions.on_cache_click);
                                move |window, cx| on_click(window, cx)
                            },
                        ),
                    )),
                ),
        )
}

/// The modal's title row: the name, and the way out.
fn header(on_close: &CloseHandler) -> gpui::Div {
    let on_close = Rc::clone(on_close);
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(RowLayout::top_size())
                .text_color(theme::text())
                .child("settings"),
        )
        .child(
            div()
                .id("settings-close")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(control::CONTROL_HEIGHT)
                .px_3()
                .rounded_sm()
                .border_2()
                .border_color(gpui::transparent_black())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .on_click(move |_, window, cx| on_close(window, cx))
                .child(tabs::icon_element(icons::Icon::Check, theme::text()))
                .child("done"),
        )
}

/// The theme section: the four names, the active one in accent.
fn theme_section(draft: &Settings, actions: &SettingsActions) -> gpui::Div {
    let chips: Vec<gpui::AnyElement> = theme::THEME_NAMES
        .iter()
        .map(|name| {
            let selected = *name == draft.theme;
            let label = (*name).to_string();
            let name = label.clone();
            let on_action = Rc::clone(&actions.on_action);
            div()
                .id(SharedString::from(format!("theme-{name}")))
                .flex()
                .flex_row()
                .items_center()
                .h(control::CONTROL_HEIGHT)
                .px_3()
                .rounded_sm()
                .cursor_pointer()
                .bg(theme::surface_alt())
                .hover(|style| style.bg(theme::surface_alt()))
                .border_2()
                .border_color(gpui::transparent_black())
                .text_size(RowLayout::small_size())
                .text_color(if selected {
                    theme::accent()
                } else {
                    theme::text_muted()
                })
                .on_click(move |_, window, cx| {
                    on_action(SettingsAction::PickTheme(name.clone()), window, cx)
                })
                .child(SharedString::from(label))
                .into_any_element()
        })
        .collect();
    section("theme", div().flex().flex_row().gap_2().children(chips))
}

/// One setting's row: its label, then its control.
fn row(label: &str, control: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .child(SharedString::from(label.to_string())),
        )
        .child(control)
}

/// A section: its muted title, then its rows.
fn section(title: &str, body: gpui::Div) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(title.to_string())),
        )
        .child(body)
}

/// A toggle: on or off, in accent or muted.
fn toggle(on: bool, action: SettingsAction, actions: &SettingsActions) -> gpui::AnyElement {
    let on_action = Rc::clone(&actions.on_action);
    let id = format!("toggle-{:?}", std::mem::discriminant(&action));
    div()
        .id(SharedString::from(id))
        .flex()
        .flex_row()
        .items_center()
        .h(control::CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .cursor_pointer()
        .bg(theme::surface_alt())
        .hover(|style| style.bg(theme::surface_alt()))
        .border_2()
        .border_color(gpui::transparent_black())
        .text_size(RowLayout::small_size())
        .text_color(if on {
            theme::accent()
        } else {
            theme::text_muted()
        })
        .on_click(move |_, window, cx| on_action(action.clone(), window, cx))
        .child(if on { "on" } else { "off" })
        .into_any_element()
}

/// A cycler: the current value, the next click takes the next one.
fn cycle_button(
    label: &str,
    action: SettingsAction,
    actions: &SettingsActions,
) -> gpui::AnyElement {
    let on_action = Rc::clone(&actions.on_action);
    let id = format!("cycle-{:?}", std::mem::discriminant(&action));
    div()
        .id(SharedString::from(id))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(control::CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .cursor_pointer()
        .bg(theme::surface_alt())
        .hover(|style| style.bg(theme::surface_alt()))
        .border_2()
        .border_color(gpui::transparent_black())
        .text_size(RowLayout::small_size())
        .text_color(theme::text())
        .on_click(move |_, window, cx| on_action(action.clone(), window, cx))
        .child(SharedString::from(label.to_string()))
        .child(tabs::icon_element(
            icons::Icon::ChevronDown,
            theme::text_muted(),
        ))
        .into_any_element()
}
