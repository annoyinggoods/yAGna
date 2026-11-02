//! The tree widget: the stored tree's text read back into its shape and
//! drawn with the directories collapsible.
//!
//! The stored text is the tree's only form in the catalog, so the
//! widget starts from that text: the core reads it back into its
//! nesting (a line with children is a directory), and the widget draws
//! the shape again — with the directories folding away in place.
//!
//! Items: `render_tree` — the tree as elements; `render_node` — one
//! node; `ToggleHandler` — what a directory's click reports.
//! Depends on: yagna_core::treefmt (the shape), gpui (the elements),
//! crate::ui::theme (the palette).
//! Called by: crate::ui::repo (the tab's structure section).

use std::rc::Rc;

use gpui::{AnyElement, App, SharedString, Window, div, prelude::*};
use yagna_core::treefmt::TreeNode;

use super::theme;

/// What a directory's click reports: the directory's path.
pub(crate) type ToggleHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// How many entries the tree draws. A big repository's structure is
/// tens of thousands of entries; the rest is a number, not a list.
pub(crate) const TREE_ENTRY_CAP: usize = 300;

/// How many entries the tree holds — the number the cap is measured
/// against, and the number the "and N more" note says.
pub(crate) fn count_entries(nodes: &[TreeNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + count_entries(&node.children))
        .sum()
}

/// The tree as elements: its roots, each with its children.
pub(crate) fn render_tree(
    nodes: &[TreeNode],
    collapsed: &std::collections::HashSet<String>,
    on_toggle: &ToggleHandler,
) -> Vec<AnyElement> {
    let mut budget = TREE_ENTRY_CAP;
    nodes
        .iter()
        .filter_map(|node| render_node(node, "", collapsed, on_toggle, &mut budget))
        .collect()
}

/// One node: its name, and — when it is open — its children.
fn render_node(
    node: &TreeNode,
    parent: &str,
    collapsed: &std::collections::HashSet<String>,
    on_toggle: &ToggleHandler,
    budget: &mut usize,
) -> Option<AnyElement> {
    let path = join_path(parent, &node.name);
    let is_directory = !node.children.is_empty();
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    if !is_directory {
        return Some(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .text_size(theme::MONO_SIZE)
                .text_color(theme::text_muted())
                .child(SharedString::from(node.name.clone()))
                .into_any_element(),
        );
    }
    let open = !collapsed.contains(&path);
    let handler = Rc::clone(on_toggle);
    let target = path.clone();
    let marker = if open { "▾" } else { "▸" };
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(
        div()
            .id(SharedString::from(format!("tree-dir-{path}")))
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
                    .child(SharedString::from(node.name.clone())),
            )
            .on_click(move |_, window, cx| handler(&target, window, cx)),
    );
    if open {
        let children: Vec<AnyElement> = node
            .children
            .iter()
            .filter_map(|child| render_node(child, &path, collapsed, on_toggle, budget))
            .collect();
        column = column.child(div().flex().flex_col().gap_1().pl_4().children(children));
    }
    Some(column.into_any_element())
}

/// The path of a node under its parent: `parent/name`.
fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        return name.to_string();
    }
    format!("{parent}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paths_nest_under_their_parents() {
        assert_eq!(join_path("", "src"), "src");
        assert_eq!(join_path("src", "deep"), "src/deep");
    }
}
