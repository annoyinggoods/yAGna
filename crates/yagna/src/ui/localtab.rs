//! The local tab: the page of a repository that lives on this machine.
//!
//! The page is the Part06 tab shape, sourced from the worktree instead
//! of the service: the metadata block (the catalog's row when the
//! origin matched one, the worktree's own facts otherwise), the reader's
//! note, the folder tree read off the disk, and a text file's preview.
//!
//! The page is read-only by construction: yAGna opens the worktree for
//! reading and never writes to it, and the page says so in one muted
//! line rather than leaving it to be discovered.
//!
//! Items: `LocalActions` — what the page's parts report; `render` — the
//! page; the entry renderers — the tree's rows and the preview.
//! Depends on: gpui (the elements), yagna_core (the local records),
//! yagna_store (the preview's shape), crate::ui::theme (the palette),
//! crate::ui::repo (the note's section, shared with the catalog's tab).
//! Called by: crate::ui (the window's root entity).

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use yagna_core::model::LocalEntry;

use super::repo::{self, StructureHandler, TabLayout};
use super::tabs::{LocalTab, Tab};
use super::theme;

/// A folder opens or closes.
pub(crate) type FolderHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A file opens: its text is read.
pub(crate) type FileHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the page's parts report.
#[derive(Clone)]
pub(crate) struct LocalActions {
    /// A folder opens or closes.
    pub on_toggle_folder: FolderHandler,
    /// A file opens: its text is read.
    pub on_open_file: FileHandler,
    /// The note is edited for this repository.
    pub on_edit_note: StructureHandler,
}

/// The page: the header, the metadata, the note, the tree, the preview,
/// and the line that says the worktree is only ever read.
pub(crate) fn render(tab: &Tab, actions: &LocalActions) -> gpui::Stateful<gpui::Div> {
    let Some(local) = tab.local.as_ref() else {
        return div()
            .id("local-tab")
            .size_full()
            .child(pending_line("this page has no worktree"));
    };
    let mut column = div().flex().flex_col().gap(TabLayout::GAP).p_4().w_full();
    column = column.child(render_header(local));
    column = column.child(render_metadata(local));
    column = column.child(render_note(local, actions));
    column = column.child(render_tree(local, actions));
    column = column.child(render_preview(local));
    column = column.child(render_read_only());
    let mut root = div()
        .id("local-scroll")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(TabLayout::SCROLLBAR)
        .track_scroll(&tab.scroll)
        .child(
            div()
                .w_full()
                .max_w(TabLayout::CONTENT_WIDTH)
                .mx_auto()
                .px_4()
                .py_4()
                .child(column),
        );
    root = root.when_some(render_missing(local), |root, missing| root.child(missing));
    root
}

/// The header: the repository's name, the path it lives at, and the
/// chip that says it is a local one.
fn render_header(local: &LocalTab) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_size(TabLayout::TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(SharedString::from(local.repo.name.to_uppercase())),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(theme::surface_alt())
                        .text_size(TabLayout::SMALL)
                        .text_color(theme::text_muted())
                        .child("local"),
                ),
        )
        .child(
            div()
                .min_w_0()
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child(SharedString::from(local.repo.path.clone())),
        )
}

/// The metadata: the worktree's own facts, and the catalog's row when
/// the origin matched one.
fn render_metadata(local: &LocalTab) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(render_section_title("metadata"));
    column = column.child(render_worktree_facts(local));
    match local.linked.as_ref() {
        Some(detail) => column = column.child(render_catalog_facts(detail)),
        None => {
            column = column.child(render_muted(
                "the catalog does not know this repository — its own worktree is all there is",
            ))
        }
    }
    column
}

/// The worktree's own facts: the branch, the last commit, the files.
fn render_worktree_facts(local: &LocalTab) -> gpui::Div {
    let branch = local
        .repo
        .branch
        .clone()
        .unwrap_or_else(|| "no branch".to_string());
    let commit = match local.repo.last_commit_at {
        Some(at) => at.format("%Y-%m-%d").to_string(),
        None => "the worktree records no commit date".to_string(),
    };
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_fact("branch", &branch));
    column = column.child(render_fact("last commit", &commit));
    column = column.child(render_fact(
        "files",
        &format!(
            "{} · {}",
            local.repo.file_count,
            human_bytes(local.repo.total_bytes)
        ),
    ));
    if let Some(origin) = local.repo.origin.as_ref() {
        column = column.child(render_fact("origin", origin));
    }
    column
}

/// The catalog's facts for the same repository, when the origin
/// matched one of its rows.
fn render_catalog_facts(detail: &yagna_core::model::RepoDetail) -> gpui::Div {
    let fit = match detail.fit.as_ref() {
        Some(fit) => format!("fit {:.1}", fit.total),
        None => "fit —".to_string(),
    };
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_fact(
        "in the catalog",
        &format!(
            "{} stars · health {}/12 · {fit}",
            detail.stars, detail.health.total
        ),
    ));
    if !detail.topics.is_empty() {
        column = column.child(render_fact("topics", &detail.topics.join(", ")));
    }
    column
}

/// The note: the reader's own words about this repository, and the way
/// in. The same section the catalog's tab shows.
fn render_note(local: &LocalTab, actions: &LocalActions) -> gpui::Div {
    let note = local.note.clone();
    repo::render_note_section(
        &format!("local-{}", local.id),
        note.as_deref(),
        &actions.on_edit_note,
    )
}

/// The tree: the worktree's folders and files, read as the reader
/// opens them.
fn render_tree(local: &LocalTab, actions: &LocalActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("files"));
    let entries = local.entries.get("").cloned().unwrap_or_default();
    if entries.is_empty() {
        return column.child(render_muted("the worktree's root is empty — or unread"));
    }
    let rows = render_entries("", &entries, local, actions);
    column = column.child(div().font_family(theme::MONO_FONT).children(rows));
    column
}

/// One folder's entries: the folders open in place, the files open
/// their preview.
fn render_entries(
    parent: &str,
    entries: &[LocalEntry],
    local: &LocalTab,
    actions: &LocalActions,
) -> Vec<AnyElement> {
    entries
        .iter()
        .filter_map(|entry| render_entry(parent, entry, local, actions))
        .collect()
}

/// One entry of the tree: a folder that opens, or a file that shows
/// its text.
fn render_entry(
    parent: &str,
    entry: &LocalEntry,
    local: &LocalTab,
    actions: &LocalActions,
) -> Option<AnyElement> {
    match entry.kind {
        yagna_core::model::LocalEntryKind::Directory => {
            Some(render_folder(parent, entry, local, actions))
        }
        yagna_core::model::LocalEntryKind::File => Some(render_file(entry, actions)),
    }
}

/// A folder: its name, its marker, and — when it is open — its
/// children.
fn render_folder(
    parent: &str,
    entry: &LocalEntry,
    local: &LocalTab,
    actions: &LocalActions,
) -> AnyElement {
    let open = local.opened.contains(&entry.path);
    let handler = Rc::clone(&actions.on_toggle_folder);
    let target = entry.path.clone();
    let marker = if open { "▾" } else { "▸" };
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(
        div()
            .id(SharedString::from(format!("local-dir-{}", entry.path)))
            .flex()
            .flex_row()
            .gap_2()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .child(
                div()
                    .text_size(theme::MONO_SIZE)
                    .text_color(theme::accent())
                    .child(SharedString::from(marker.to_string())),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::MONO_SIZE)
                    .text_color(theme::text())
                    .child(SharedString::from(entry.name.clone())),
            )
            .on_click(move |_, window, cx| handler(&target, window, cx)),
    );
    if !open {
        return column.into_any_element();
    }
    let children = local.entries.get(&entry.path).cloned().unwrap_or_default();
    let rows = render_entries(&entry.path, &children, local, actions);
    let _ = parent;
    column = column.child(div().flex().flex_col().gap_1().pl_4().children(rows));
    column.into_any_element()
}

/// A file: its name and its size, and a click opens its text.
fn render_file(entry: &LocalEntry, actions: &LocalActions) -> AnyElement {
    let handler = Rc::clone(&actions.on_open_file);
    let target = entry.path.clone();
    let size = entry
        .size
        .map(human_bytes)
        .unwrap_or_else(|| "—".to_string());
    div()
        .id(SharedString::from(format!("local-file-{}", entry.path)))
        .flex()
        .flex_row()
        .gap_2()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .child(
            div()
                .font_family(theme::MONO_FONT)
                .text_size(theme::MONO_SIZE)
                .text_color(theme::text_muted())
                .child(SharedString::from(entry.name.clone())),
        )
        .child(
            div()
                .text_size(theme::MONO_SIZE)
                .text_color(theme::text_muted())
                .child(SharedString::from(size)),
        )
        .on_click(move |_, window, cx| handler(&target, window, cx))
        .into_any_element()
}

/// The preview: the opened file's text, or the fact that it has none
/// to show.
fn render_preview(local: &LocalTab) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("preview"));
    let Some(preview) = local.preview.as_ref() else {
        return column.child(render_muted("no file is open"));
    };
    let path = local.preview_path.clone().unwrap_or_default();
    column = column.child(render_muted(&format!(
        "{path} · {}",
        human_bytes(preview.bytes)
    )));
    if preview.binary {
        return column.child(render_muted("a binary file — its bytes are not text"));
    }
    column = column.child(
        div()
            .id("local-preview")
            .max_h(px(420.0))
            .overflow_y_scroll()
            .scrollbar_width(TabLayout::SCROLLBAR)
            .font_family(theme::MONO_FONT)
            .text_size(theme::MONO_SIZE)
            .text_color(theme::text())
            .child(SharedString::from(preview.text.clone())),
    );
    if preview.truncated {
        column = column.child(render_muted(
            "the file was longer than the cap — this is its head",
        ));
    }
    column
}

/// The line that says the worktree is only ever read.
fn render_read_only() -> gpui::Div {
    render_muted("yAGna reads this worktree — it never writes to it")
}

/// The line a folder that went away leaves behind.
fn render_missing(local: &LocalTab) -> Option<gpui::Stateful<gpui::Div>> {
    local.repo.missing_at.map(|_| {
        div()
            .id("local-missing")
            .absolute()
            .bottom_3()
            .right_3()
            .px_3()
            .py_2()
            .rounded_sm()
            .bg(theme::notice_bg())
            .text_size(TabLayout::SMALL)
            .text_color(theme::notice_text())
            .child("this folder is gone — the entry is kept, in case it comes back")
    })
}

/// One dated fact: its label and its value.
fn render_fact(label: &str, value: &str) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .child(
            div()
                .w(TabLayout::LABEL_WIDTH)
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .min_w_0()
                .text_size(TabLayout::BODY)
                .text_color(theme::text())
                .child(SharedString::from(value.to_string())),
        )
}

/// A muted line: the small print, a fact, never an alarm.
fn render_muted(text: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

/// A section's title.
fn render_section_title(title: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(SharedString::from(title.to_uppercase()))
}

/// The one line that says what is not there yet.
fn pending_line(text: &str) -> gpui::Div {
    render_muted(text)
}

/// A byte count, in the units a reader reads.
fn human_bytes(bytes: u64) -> String {
    const UNIT: f64 = 1024.0;
    let size = bytes as f64;
    if size < UNIT {
        return format!("{bytes} B");
    }
    if size < UNIT * UNIT {
        return format!("{:.1} KB", size / UNIT);
    }
    if size < UNIT * UNIT * UNIT {
        return format!("{:.1} MB", size / (UNIT * UNIT));
    }
    format!("{:.1} GB", size / (UNIT * UNIT * UNIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_counts_are_read_the_way_a_reader_reads_them() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
