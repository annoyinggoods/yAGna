//! The in-app menu: the File, View and Help heads, and the items under
//! each.
//!
//! The window has no native menu bar, so the menu is a row of heads
//! at the top of the shell: a head opens a dropdown, an item asks for
//! a command, and the pane — which owns the commands — decides what
//! happens. The menu never performs an action itself; it names one.
//!
//! Items: `Head` — the three heads; `Command` — what an item asks
//! for; `MenuActions` — the closures the pane hands in; `render_row` —
//! the row and the dropdown under the open head.
//! Depends on: gpui (the elements), crate::ui::theme (the inks),
//! crate::ui::rows (the type ramp).
//! Called by: crate::ui (the shell's top row).

use std::rc::Rc;

use gpui::{App, SharedString, Window, div, prelude::*, px};

use super::icons::Icon;
use super::rows::RowLayout;
use super::tabs::icon_element;
use super::theme;

/// The dropdown's offset from the head it hangs under: the head
/// button's own height, plus the gap the row leaves.
const DROP_TOP: f32 = 32.0;
/// The dropdown's width.
const DROP_WIDTH: gpui::Pixels = px(210.0);

/// The menu's heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Head {
    /// What the session does with the catalog.
    File,
    /// What the window shows.
    View,
    /// What the app is, and where to complain.
    Help,
}

impl Head {
    /// Every head, in the order the row shows them.
    pub(crate) const ALL: [Head; 3] = [Head::File, Head::View, Head::Help];

    /// The name the head wears.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Head::File => "File",
            Head::View => "View",
            Head::Help => "Help",
        }
    }

    /// The items under the head.
    pub(crate) fn items(self) -> &'static [Item] {
        match self {
            Head::File => &FILE_ITEMS,
            Head::View => &VIEW_ITEMS,
            Head::Help => &HELP_ITEMS,
        }
    }
}

/// What an item asks for. The pane owns the meaning of each one; the
/// menu only names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// Sweep the starred repositories into the catalog.
    UpdateRepositories,
    /// Pull the filtered list into the cache.
    PrefetchFiltered,
    /// Write the catalog out.
    ExportCatalog,
    /// Merge an exported catalog back in.
    ImportCatalog,
    /// Leave the app.
    Quit,
    /// Move to the next theme.
    CycleTheme,
    /// Paint the type larger.
    IncreaseTextSize,
    /// Paint the type smaller.
    DecreaseTextSize,
    /// Show or hide the tag sidebar.
    ToggleSidebar,
    /// Open the About card.
    About,
    /// Open the shortcuts card.
    Shortcuts,
    /// Open the issue tracker.
    ReportIssue,
}

/// One menu item: the label it wears, and the command it asks for.
pub(crate) struct Item {
    /// The label the item wears.
    pub label: &'static str,
    /// The command the item asks for.
    pub command: Command,
    /// The shape the item wears, when it wears one: the items that
    /// leave the app carry the arrow.
    pub icon: Option<Icon>,
}

/// The File head's items.
const FILE_ITEMS: [Item; 5] = [
    Item {
        label: "Update repositories",
        command: Command::UpdateRepositories,
        icon: None,
    },
    Item {
        label: "Pre-fetch the filtered list",
        command: Command::PrefetchFiltered,
        icon: None,
    },
    Item {
        label: "Export the catalog",
        command: Command::ExportCatalog,
        icon: None,
    },
    Item {
        label: "Import a catalog",
        command: Command::ImportCatalog,
        icon: None,
    },
    Item {
        label: "Quit",
        command: Command::Quit,
        icon: None,
    },
];

/// The View head's items.
const VIEW_ITEMS: [Item; 4] = [
    Item {
        label: "Cycle the theme",
        command: Command::CycleTheme,
        icon: None,
    },
    Item {
        label: "Increase the text size",
        command: Command::IncreaseTextSize,
        icon: None,
    },
    Item {
        label: "Decrease the text size",
        command: Command::DecreaseTextSize,
        icon: None,
    },
    Item {
        label: "Toggle the tag sidebar",
        command: Command::ToggleSidebar,
        icon: None,
    },
];

/// The Help head's items.
const HELP_ITEMS: [Item; 3] = [
    Item {
        label: "About yAGna",
        command: Command::About,
        icon: None,
    },
    Item {
        label: "Shortcuts",
        command: Command::Shortcuts,
        icon: None,
    },
    Item {
        label: "Report an issue",
        command: Command::ReportIssue,
        icon: Some(Icon::ArrowUpRight),
    },
];

/// What an item's click calls: the command it stands for.
pub(crate) type CommandHandler = Rc<dyn Fn(Command, &mut Window, &mut App)>;

/// What a head's click calls: the head that was clicked.
pub(crate) type HeadHandler = Rc<dyn Fn(Head, &mut Window, &mut App)>;

/// The closures the pane hands the menu.
#[derive(Clone)]
pub(crate) struct MenuActions {
    /// An item was clicked.
    pub on_command: CommandHandler,
    /// A head was clicked: it opens, or closes when it is already open.
    pub on_head: HeadHandler,
}

/// The menu row: the three heads, and the dropdown under the open one.
pub(crate) fn render_row(open: Option<Head>, actions: &MenuActions) -> gpui::Div {
    let heads: Vec<gpui::AnyElement> = Head::ALL
        .iter()
        .map(|head| render_head(*head, open == Some(*head), actions))
        .collect();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .children(heads)
}

/// One head: the button, and the dropdown under it when it is open.
fn render_head(head: Head, is_open: bool, actions: &MenuActions) -> gpui::AnyElement {
    let on_head = Rc::clone(&actions.on_head);
    let button = div()
        .id(SharedString::from(format!("menu-head-{}", head.label())))
        .px_2()
        .py_1()
        .rounded_sm()
        .text_size(RowLayout::small_size())
        .text_color(if is_open {
            theme::accent()
        } else {
            theme::text()
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |_, window, cx| on_head(head, window, cx))
        .child(head.label());
    let mut column = div().relative().child(button);
    if is_open {
        column = column.child(render_dropdown(head, actions));
    }
    column.into_any_element()
}

/// The dropdown under a head: its items, one per row.
fn render_dropdown(head: Head, actions: &MenuActions) -> gpui::Div {
    let items: Vec<gpui::AnyElement> = head
        .items()
        .iter()
        .map(|item| render_item(item, actions))
        .collect();
    div()
        .absolute()
        .top(px(DROP_TOP * super::typography::scale()))
        .left_0()
        .w(DROP_WIDTH)
        .rounded_sm()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .children(items)
}

/// One item: its label, and the command it asks for.
fn render_item(item: &Item, actions: &MenuActions) -> gpui::AnyElement {
    let on_command = Rc::clone(&actions.on_command);
    let command = item.command;
    div()
        .id(SharedString::from(format!("menu-item-{}", item.label)))
        .px_3()
        .py_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::text())
        .on_click(move |_, window, cx| on_command(command, window, cx))
        .when_some(item.icon, |row, icon| {
            row.child(icon_element(icon, theme::text_muted()))
        })
        .child(item.label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every head carries items, and every item names a command: a
    /// head with nothing under it would open an empty card.
    #[test]
    fn every_head_carries_items_with_commands() {
        for head in Head::ALL {
            assert!(!head.items().is_empty(), "{} has items", head.label());
            for item in head.items() {
                assert!(!item.label.is_empty());
            }
        }
    }

    /// The labels are the ones the menu shows: no head repeats an
    /// item, and no item repeats a label.
    #[test]
    fn the_items_are_distinct() {
        let mut labels: Vec<&str> = Vec::new();
        for head in Head::ALL {
            for item in head.items() {
                assert!(!labels.contains(&item.label), "{} twice", item.label);
                labels.push(item.label);
            }
        }
        assert_eq!(labels.len(), 12, "twelve items over three heads");
    }

    /// The heads are named the way the row shows them.
    #[test]
    fn the_heads_are_named() {
        assert_eq!(Head::File.label(), "File");
        assert_eq!(Head::View.label(), "View");
        assert_eq!(Head::Help.label(), "Help");
    }
}
