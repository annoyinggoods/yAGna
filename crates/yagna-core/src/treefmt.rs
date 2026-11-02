//! The pure tree format: the flat recursive listing a forge returns,
//! rendered as the box-drawing text tree the repository tab displays.
//!
//! Directories sort before files, both case-insensitively; the depth is
//! capped with an ellipsis marker for the deep middle; a final line
//! reports the forge's own truncation flag — the structure may be
//! incomplete and the fact is visible, never swallowed. Submodule
//! entries (`TreeEntryKind::Commit`) are skipped: nobody renders them.
//!
//! Items: `render` — the flat listing to text; `parse` — the rendered
//! text back to its shape; `MAX_RENDER_DEPTH` — the depth cap; `Node`/
//! `build_forest` — the internal hierarchy the renderer walks; `TreeNode`
//! — one node of a parsed tree.
//! Depends on: crate::model (TreeEntry, the shape the tree endpoint
//! returns).
//! Called by: yagna-store (which renders at storage time), the forge
//! (whose tree mapping produces the input) and the app (whose tree
//! widget reads the rendered text back).

use crate::model::{TreeEntry, TreeEntryKind};

/// How deep the rendered tree descends before the middle collapses into
/// an ellipsis marker.
pub const MAX_RENDER_DEPTH: usize = 10;

/// The deepest path the forest accepts: the build, the sort and the
/// drop all recurse over the nesting, so a path deeper than this is
/// dropped instead of walked (git tree objects enforce no PATH_MAX of
/// their own).
pub const MAX_PATH_SEGMENTS: usize = 512;

/// The closing line a set-forge truncation flag renders.
pub const TRUNCATION_LINE: &str = "… (structure truncated by the forge)";

/// One node of the internal hierarchy: a name within its parent and the
/// children beneath it (empty for files).
#[derive(Debug)]
struct Node {
    name: String,
    dir: bool,
    children: Vec<Node>,
}

/// Renders the flat listing as the text tree. The input order does not
/// matter: the hierarchy is rebuilt from the paths, then sorted
/// (directories first, names case-insensitive). `truncated` appends the
/// forge's own "structure incomplete" line.
pub fn render(entries: &[TreeEntry], truncated: bool) -> String {
    let mut out = String::new();
    render_forest(&build_forest(entries), "", 0, &mut out);
    if truncated {
        out.push_str(TRUNCATION_LINE);
        out.push('\n');
    }
    out
}

/// Builds the top-level nodes from the flat paths: every intermediate
/// segment is a directory, the last segment takes the entry's kind.
/// Submodule entries never enter the tree.
fn build_forest(entries: &[TreeEntry]) -> Vec<Node> {
    let mut forest: Vec<Node> = Vec::new();
    for entry in entries {
        if entry.kind == TreeEntryKind::Commit {
            continue;
        }
        insert_path(&mut forest, &entry.path, entry.kind == TreeEntryKind::Tree);
    }
    sort_siblings(&mut forest);
    forest
}

/// Inserts one path into the forest, creating the directory nodes along
/// the way. A path with more segments than the cap is dropped, not
/// walked: the build, the sort and the drop are recursive over the
/// nesting, and a hostile path must not buy stack with its length.
fn insert_path(forest: &mut Vec<Node>, path: &str, dir: bool) {
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else {
        return;
    };
    if segments.len() > MAX_PATH_SEGMENTS {
        return;
    }
    let mut children = forest;
    for parent in parents {
        let index = find_or_create(children, parent, true);
        children = &mut children[index].children;
    }
    find_or_create(children, last, dir);
}

/// The index of the child named `name`, created as a directory when
/// absent (a later file entry at the same path would be a forge
/// contradiction; the directory marker then simply loses).
fn find_or_create(children: &mut Vec<Node>, name: &str, dir: bool) -> usize {
    if let Some(index) = children.iter().position(|node| node.name == name) {
        return index;
    }
    children.push(Node {
        name: name.to_string(),
        dir,
        children: Vec::new(),
    });
    children.len() - 1
}

/// Sorts every level in place: directories first, then names,
/// case-insensitively (stable, so equal lowercase names keep their
/// input order).
fn sort_siblings(nodes: &mut [Node]) {
    nodes.sort_by(|left, right| {
        left.dir
            .cmp(&right.dir)
            .reverse()
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    for node in nodes {
        sort_siblings(&mut node.children);
    }
}

/// Renders the whole forest at depth 0.
fn render_forest(nodes: &[Node], prefix: &str, depth: usize, out: &mut String) {
    for (index, node) in nodes.iter().enumerate() {
        let last = index + 1 == nodes.len();
        render_node(node, prefix, depth, last, out);
    }
}

/// One node and, unless the depth cap hits, its children.
fn render_node(node: &Node, prefix: &str, depth: usize, last: bool, out: &mut String) {
    out.push_str(prefix);
    out.push_str(if last { "└── " } else { "├── " });
    out.push_str(&node.name);
    out.push('\n');
    if node.children.is_empty() {
        return;
    }
    let mut child_prefix = String::from(prefix);
    child_prefix.push_str(if last { "    " } else { "│   " });
    if depth + 1 < MAX_RENDER_DEPTH {
        render_forest(&node.children, &child_prefix, depth + 1, out);
        return;
    }
    child_prefix.push_str("└── …\n");
    out.push_str(&child_prefix);
}

/// One node of a parsed tree: its name and its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub name: String,
    pub children: Vec<TreeNode>,
}

/// The rendered tree read back into its shape: the flat text's lines are
/// nested by their indentation, and a line with children is a directory.
/// The inverse of [`render`] — the tab's tree widget draws the shape
/// again, this time with the directories collapsible.
///
/// The rendered text is the only input: the renderer draws a directory
/// and a file the same way, so a leaf is a file and a node with
/// children is a directory. An empty directory reads as a leaf.
pub fn parse(rendered: &str) -> Vec<TreeNode> {
    let flat: Vec<Flat> = rendered.lines().filter_map(flat_line).collect();
    let mut at = 0usize;
    assemble(&flat, &mut at, 0)
}

/// One line of the rendered text: its depth in the tree and its name.
struct Flat {
    depth: usize,
    name: String,
}

/// One rendered line as a depth and a name, when the line is a tree
/// line at all (the truncation marker is not). The depth is the number
/// of indentation groups in front of the marker — each group is four
/// columns wide, and a group is either a bar or blanks.
fn flat_line(line: &str) -> Option<Flat> {
    let mut rest = line;
    let mut depth = 0usize;
    while let Some(tail) = rest.strip_prefix("│   ") {
        depth += 1;
        rest = tail;
    }
    while let Some(tail) = rest.strip_prefix("    ") {
        depth += 1;
        rest = tail;
    }
    let name = rest
        .strip_prefix("├── ")
        .or_else(|| rest.strip_prefix("└── "))?;
    Some(Flat {
        depth,
        name: name.to_string(),
    })
}

/// The nodes of one level, taken from the flat list.
fn assemble(flat: &[Flat], at: &mut usize, depth: usize) -> Vec<TreeNode> {
    let mut nodes = Vec::new();
    while *at < flat.len() {
        let line = &flat[*at];
        if line.depth != depth {
            break;
        }
        *at += 1;
        let children = assemble(flat, at, depth + 1);
        nodes.push(TreeNode {
            name: line.name.clone(),
            children,
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: TreeEntryKind) -> TreeEntry {
        TreeEntry {
            path: path.to_string(),
            kind,
            size: None,
        }
    }

    fn sample() -> Vec<TreeEntry> {
        vec![
            entry("README.md", TreeEntryKind::Blob),
            entry("src", TreeEntryKind::Tree),
            entry("src/main.rs", TreeEntryKind::Blob),
            entry("src/deep/mod.rs", TreeEntryKind::Blob),
            entry("tests", TreeEntryKind::Tree),
            entry("tests/it.rs", TreeEntryKind::Blob),
            entry(".gitignore", TreeEntryKind::Blob),
        ]
    }

    #[test]
    fn renders_directories_first_case_insensitively() {
        let text = render(&sample(), false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "├── src",
                "│   ├── deep",
                "│   │   └── mod.rs",
                "│   └── main.rs",
                "├── tests",
                "│   └── it.rs",
                "├── .gitignore",
                "└── README.md",
            ]
        );
    }

    #[test]
    fn submodules_are_skipped_not_rendered() {
        let mut entries = sample();
        entries.push(entry("vendor/libgit2", TreeEntryKind::Commit));
        assert_eq!(render(&entries, false), render(&sample(), false));
    }

    #[test]
    fn the_input_order_does_not_matter() {
        let shuffled: Vec<TreeEntry> = sample().into_iter().rev().collect();
        assert_eq!(render(&shuffled, false), render(&sample(), false));
    }

    #[test]
    fn unicode_and_dotfiles_sort_plainly() {
        let entries = vec![
            entry("Ärger.txt", TreeEntryKind::Blob),
            entry("zeit.txt", TreeEntryKind::Blob),
            entry(".env", TreeEntryKind::Blob),
            entry("assets", TreeEntryKind::Tree),
        ];
        let text = render(&entries, false);
        let names: Vec<&str> = text
            .lines()
            .map(|line| line.trim_start_matches("├── ").trim_start_matches("└── "))
            .collect();
        assert_eq!(
            names,
            ["assets", ".env", "zeit.txt", "Ärger.txt"],
            "codepoint order: 'z' (122) sorts before 'ä' (228); names keep their case"
        );
    }

    #[test]
    fn a_wide_repo_renders_every_sibling() {
        let entries: Vec<TreeEntry> = (0..500)
            .map(|index| entry(&format!("module_{index:03}.rs"), TreeEntryKind::Blob))
            .collect();
        let text = render(&entries, false);
        assert_eq!(text.lines().count(), 500);
        assert!(text.starts_with("├── module_000.rs\n"));
        assert!(text.ends_with("└── module_499.rs\n"));
    }

    #[test]
    fn the_deep_middle_collapses_into_an_ellipsis_marker() {
        let mut path = String::new();
        for level in 0..MAX_RENDER_DEPTH + 3 {
            path.push_str(&format!("level{level}/"));
        }
        path.push_str("bottom.rs");
        let entries = vec![entry(&path, TreeEntryKind::Blob)];
        let text = render(&entries, false);
        assert!(
            text.contains("└── …\n"),
            "the deep middle is marked: {text}"
        );
        assert!(!text.contains("bottom.rs"), "nothing renders past the cap");
        assert_eq!(text.lines().count(), MAX_RENDER_DEPTH + 1);
    }

    #[test]
    fn the_truncated_flag_becomes_the_closing_line() {
        let text = render(&sample(), true);
        assert!(text.ends_with("… (structure truncated by the forge)\n"));
        assert_eq!(text.lines().count(), 9);

        assert_eq!(render(&[], true), "… (structure truncated by the forge)\n");
        assert_eq!(render(&[], false), "");
    }

    #[test]
    fn the_rendered_tree_reads_back_into_its_shape() {
        let nodes = parse(&render(&sample(), false));
        let names: Vec<&str> = nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, ["src", "tests", ".gitignore", "README.md"]);
        assert!(
            nodes[0].children.iter().any(|node| node.name == "deep"),
            "the directories carry their children"
        );
        assert!(nodes[3].children.is_empty(), "the file is a leaf");
    }

    #[test]
    fn the_parse_ignores_the_truncation_marker() {
        let nodes = parse(&render(&sample(), true));
        assert_eq!(nodes.len(), 4);
        assert!(
            !nodes.iter().any(|node| node.name.contains("truncated")),
            "the marker is not a node"
        );
    }

    #[test]
    fn an_empty_tree_parses_into_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("… (structure truncated by the forge)\n").is_empty());
    }

    #[test]
    fn a_pathologically_deep_path_is_dropped_not_walked() {
        let hostile = format!("a/{}.txt", "b/".repeat(100_000));
        let entry = TreeEntry {
            path: hostile,
            kind: TreeEntryKind::Blob,
            size: None,
        };
        let rendered = render(&[entry], false);
        assert!(
            !rendered.contains("truncated by the forge"),
            "the hostile path never reaches the renderer"
        );
    }
}
