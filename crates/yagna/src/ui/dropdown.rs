//! The dropdown: the one control every picker in the app opens — the
//! deep-search builder's field and operator, DISCOVER's sort, and the
//! library's sort.
//!
//! A click on the trigger opens a list of the options (the ones the
//! grammar or the query accepts), a pick closes the list and writes
//! the choice, and the open list is keyboard-operable: the arrows walk
//! the options, `enter` picks, `esc` closes. The trigger's own label
//! announces the open list (`▾` closed, `▴` open) — the reader can see
//! the state without remembering it.
//!
//! One pane-level open state means never two dropdowns at once; the
//! pane's click-outside rule closes an open list on any click that is
//! not inside it, because the trigger and every option stop the click
//! from bubbling past this component.
//!
//! Items: `Site` — the four pickers; `DropdownOption` — one option's
//! label and whether it is the chosen one; `DropdownActions` — what
//! the control reports; `render_inline` — the trigger with the list
//! under it, in the flow; `render_popup` — the list alone, for the
//! header's absolutely-anchored picker.
//! Depends on: gpui (the elements, the focus handle), crate::ui::theme
//! (the palette), crate::ui::control (the target height).
//! Called by: crate::ui (the builder, the DISCOVER controls, the
//! library header).

use std::rc::Rc;

use gpui::{App, FocusHandle, KeyDownEvent, SharedString, Window, div, prelude::*, px};

use super::control::CONTROL_HEIGHT;
use super::rows::RowLayout;
use super::theme;

/// The dropdown's width: wide enough for the longest option the
/// grammar offers.
const LIST_WIDTH: gpui::Pixels = px(190.0);

/// The four pickers that open a dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    /// The deep-search builder's field picker.
    BuilderField,
    /// The deep-search builder's operator picker.
    BuilderOp,
    /// DISCOVER's sort.
    DiscoverSort,
    /// The library's sort.
    LibrarySort,
}

/// One option of an open dropdown: its label, and whether it is the
/// choice the control holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropdownOption {
    pub label: String,
    pub selected: bool,
}

impl DropdownOption {
    /// One option, from a label and its selected state.
    pub(crate) fn new(label: impl Into<String>, selected: bool) -> Self {
        Self {
            label: label.into(),
            selected,
        }
    }
}

/// The trigger was pressed: the picker it belongs to opens or closes.
pub(crate) type OpenHandler = Rc<dyn Fn(Site, &mut Window, &mut App)>;
/// An option was picked: the picker and the option's index ride along.
pub(crate) type PickHandler = Rc<dyn Fn(Site, usize, &mut Window, &mut App)>;
/// A keystroke reached the trigger of the picker named.
pub(crate) type KeyHandler = Rc<dyn Fn(Site, &KeyDownEvent, &mut Window, &mut App)>;

/// What the dropdown's parts report.
pub(crate) struct DropdownActions {
    /// The trigger was pressed.
    pub on_open: OpenHandler,
    /// An option was picked, by index.
    pub on_pick: PickHandler,
    /// A keystroke reached the trigger — the open list's keyboard map.
    pub on_key: KeyHandler,
}

/// The picker: the trigger button, and the option list under it when
/// it is open, in the flow. `highlight` is the option the keyboard is
/// on; `focus` is the trigger's handle, so an open list can be walked
/// without the pointer.
pub(crate) fn render_inline(
    site: Site,
    label: &str,
    open: bool,
    highlight: usize,
    options: &[DropdownOption],
    focus: &FocusHandle,
    actions: &DropdownActions,
) -> gpui::AnyElement {
    let trigger = render_trigger(site, label, open, focus, actions);
    let mut column = div().flex().flex_col().child(trigger);
    if open {
        column = column.child(render_list(site, highlight, options, actions));
    }
    column.into_any_element()
}

/// The trigger button: the control's current label, the chevron that
/// announces the open list, and the keyboard map of the open list.
fn render_trigger(
    site: Site,
    label: &str,
    open: bool,
    focus: &FocusHandle,
    actions: &DropdownActions,
) -> gpui::Stateful<gpui::Div> {
    let on_open = Rc::clone(&actions.on_open);
    let on_key = Rc::clone(&actions.on_key);
    div()
        .id(SharedString::from(format!("dropdown-{}", site.name())))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .track_focus(focus)
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(if open { theme::accent() } else { theme::text() })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_open(site, window, cx);
        })
        .on_key_down(move |event, window, cx| on_key(site, event, window, cx))
        .child(SharedString::from(label.to_string()))
        .child(SharedString::from(if open { " ▴" } else { " ▾" }))
}

/// The open list: one row per option, the chosen one in the accent,
/// the keyboard's row highlighted. The list stops its own clicks from
/// reaching the pane's click-outside rule.
fn render_list(
    site: Site,
    highlight: usize,
    options: &[DropdownOption],
    actions: &DropdownActions,
) -> gpui::Stateful<gpui::Div> {
    let rows: Vec<gpui::AnyElement> = options
        .iter()
        .enumerate()
        .map(|(index, option)| render_option(site, index, option, highlight == index, actions))
        .collect();
    div()
        .id(SharedString::from(format!("dropdown-list-{}", site.name())))
        .flex()
        .flex_col()
        .gap_1()
        .p_1()
        .rounded_sm()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .on_click(|_, _, cx| cx.stop_propagation())
        .children(rows)
}

/// The popup list for the header's picker: the same list, unanchored —
/// the caller positions it (the header's geometry lives with the
/// header).
pub(crate) fn render_popup(
    site: Site,
    highlight: usize,
    options: &[DropdownOption],
    actions: &DropdownActions,
) -> gpui::Stateful<gpui::Div> {
    render_list(site, highlight, options, actions).w(LIST_WIDTH)
}

/// One option row: the label, the chosen one in the accent with its
/// mark, the keyboard's row highlighted.
fn render_option(
    site: Site,
    index: usize,
    option: &DropdownOption,
    highlighted: bool,
    actions: &DropdownActions,
) -> gpui::AnyElement {
    let on_pick = Rc::clone(&actions.on_pick);
    div()
        .id(SharedString::from(format!(
            "dropdown-option-{}-{index}",
            site.name()
        )))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .when(highlighted, |row| row.bg(theme::surface_alt()))
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(if option.selected {
            theme::accent()
        } else {
            theme::text()
        })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_pick(site, index, window, cx);
        })
        .child(SharedString::from(format!(
            "{}{}",
            if option.selected { "✓ " } else { "" },
            option.label
        )))
        .into_any_element()
}

impl Site {
    /// The picker's element-id name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Site::BuilderField => "builder-field",
            Site::BuilderOp => "builder-op",
            Site::DiscoverSort => "discover-sort",
            Site::LibrarySort => "library-sort",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four pickers name themselves distinctly — the element ids
    /// must not collide.
    #[test]
    fn the_sites_name_themselves_distinctly() {
        let names: Vec<&str> = [
            Site::BuilderField,
            Site::BuilderOp,
            Site::DiscoverSort,
            Site::LibrarySort,
        ]
        .iter()
        .map(|site| site.name())
        .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "no two sites share a name");
    }

    /// An option carries its label and its chosen state, built in one
    /// call.
    #[test]
    fn an_option_carries_its_label_and_its_choice() {
        let option = DropdownOption::new("stars", true);
        assert_eq!(option.label, "stars");
        assert!(option.selected);
        assert_eq!(
            DropdownOption::new("newest", false),
            DropdownOption {
                label: "newest".to_string(),
                selected: false,
            }
        );
    }
}
