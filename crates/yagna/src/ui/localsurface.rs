//! The local surface: the repositories that live on this machine.
//!
//! The surface lists the worktrees the scan found under the folder the
//! reader named, each with the facts the worktree itself carries and
//! the catalog's row when the origin matched one. It is the way in to
//! a local page, and the place the scan is asked for.
//!
//! Items: `LocalActions` — what the surface's parts report; `render` —
//! the surface; `render_entry` — one discovered repository.
//! Depends on: gpui (the elements), yagna_core (the local record),
//! crate::ui::theme (the palette).
//! Called by: crate::ui (the window's root entity).

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*};
use yagna_core::model::LocalRepo;

use super::rows::RowLayout;
use super::theme;

/// The scan is asked for.
pub(crate) type ScanHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A discovered repository is opened: its page comes up.
pub(crate) type OpenHandler = Rc<dyn Fn(i64, &mut Window, &mut App)>;

/// What the surface's parts report.
#[derive(Clone)]
pub(crate) struct LocalActions {
    /// The folder is scanned again.
    pub on_scan: ScanHandler,
    /// A discovered repository is opened.
    pub on_open: OpenHandler,
}

/// The surface: the folder, the scan, and the repositories under it —
/// one scrollable column, however many worktrees the scan found.
pub(crate) fn render(
    repos: &[LocalRepo],
    folder: Option<&str>,
    scanning: bool,
    actions: &LocalActions,
) -> gpui::Stateful<gpui::Div> {
    let mut column = div()
        .id("local-surface")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(super::repo::TabLayout::SCROLLBAR)
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .w_full()
        .bg(theme::background())
        .text_color(theme::text());
    column = column.child(render_header(folder, scanning, actions));
    if repos.is_empty() {
        return column.child(render_quiet(
            "no repository has been found under that folder",
        ));
    }
    let rows: Vec<gpui::AnyElement> = repos
        .iter()
        .map(|repo| render_entry(repo, actions))
        .collect();
    column.child(div().flex().flex_col().gap_1().children(rows))
}

/// The header: the surface's name, the folder the scan reads, and the
/// scan itself.
fn render_header(folder: Option<&str>, scanning: bool, actions: &LocalActions) -> gpui::Div {
    let handler = Rc::clone(&actions.on_scan);
    let scan = div()
        .id("local-scan")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| handler(window, cx))
        .child(if scanning {
            "scanning…"
        } else {
            "scan again"
        });
    let folder_line = match folder {
        Some(path) => path.to_string(),
        None => "no folder is set — name one in the settings".to_string(),
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(RowLayout::top_size())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("local repositories"),
                )
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child(SharedString::from(folder_line)),
                ),
        )
        .child(scan)
}

/// One discovered repository: its name, its path, its own facts, and
/// whether the catalog knows it.
fn render_entry(repo: &LocalRepo, actions: &LocalActions) -> gpui::AnyElement {
    let handler = Rc::clone(&actions.on_open);
    let id = repo.id;
    let mut facts: Vec<String> = Vec::new();
    if let Some(branch) = repo.branch.as_ref() {
        facts.push(branch.clone());
    }
    facts.push(format!("{} files", repo.file_count));
    let link = match repo.linked_rowid {
        Some(_) => "in the catalog".to_string(),
        None => "not in the catalog".to_string(),
    };
    let mut column = div().flex().flex_col().gap_1();
    if repo.missing_at.is_some() {
        column = column.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::notice_text())
                .child("this folder is gone — the entry is kept, in case it comes back"),
        );
    }
    let row = div()
        .id(SharedString::from(format!("local-entry-{id}")))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |_, window, cx| handler(id, window, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text())
                        .child(SharedString::from(repo.name.clone())),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child(SharedString::from(repo.path.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child(SharedString::from(facts.join(" · "))),
                )
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::accent())
                        .child(SharedString::from(link)),
                ),
        );
    column = column.child(row);
    column.into_any_element()
}

/// The quiet line that says there is nothing here.
fn render_quiet(text: &str) -> gpui::Div {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}
