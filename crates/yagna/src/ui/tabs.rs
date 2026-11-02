//! The tab shell: the open tabs, the focused one, and the bar that
//! shows them.
//!
//! A tab is a reader, not a workspace: it holds the repository it reads
//! and the view state that belongs to that repository (how far it was
//! scrolled, which license text is open, which directories are
//! collapsed). It holds no catalog state of its own — the tab's content
//! comes from the store through the facade, every time it is asked for.
//!
//! Opening a repository that is already open focuses the tab it is
//! already in: the tabs are views over stored data, never copies of it.
//!
//! The bar carries five permanent tabs — the library, the discovery
//! surface, the people surface, the reader's own numbers and the local
//! repositories — and the repository tabs after them. The permanent
//! ones are always there: they are the way back, and closing them is
//! not a thing the reader can do.
//!
//! Items: `TabState` — the open tabs and the focused one (plus the
//! permanent surfaces); `Surface` — the permanent tabs; `Tab` — one
//! tab's repository and its view state; `LocalTab` — what a tab over a
//! worktree holds; `TabData` — what the store gave the tab;
//! `TabActions` — what the bar's parts report; `render_bar` —
//! the bar's element.
//! Depends on: gpui (the scroll handle, the elements), yagna_core (the
//! repository identity), yagna_store (the records the tab renders).
//! Called by: crate::ui (the window's root entity).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, ClickEvent, MouseButton, ScrollHandle, SharedString, Window, div, prelude::*};
use yagna_core::mdrender::{Block, Resolved};
use yagna_core::model::{LocalEntry, LocalRepo, RepoDetail, RepoRef};
use yagna_store::{DigestBody, LocalPreview, ReadmeBody, TreeBody};

use super::icons::Icon;
use super::rows::RowLayout;
use super::theme;

/// The permanent tabs: the surfaces that are always in the bar, whether
/// a repository is open or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Surface {
    /// The catalog: the rows, the search and the tags.
    #[default]
    Library,
    /// The discovery surface: the search API's one query shape.
    Discover,
    /// The people surface: the followed logins and their shadows.
    People,
    /// The reader's own numbers: the catalog's aggregates.
    User,
    /// The local repositories: the clones on this machine.
    Local,
    /// The score surface: the fit spec and the two score spreads.
    Score,
}

impl Surface {
    /// Every permanent tab, in the order the bar shows them.
    pub(crate) const ALL: [Surface; 6] = [
        Surface::Library,
        Surface::Discover,
        Surface::People,
        Surface::User,
        Surface::Local,
        Surface::Score,
    ];

    /// The name the bar shows.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Surface::Library => "library",
            Surface::Discover => "discover",
            Surface::People => "people",
            Surface::User => "you",
            Surface::Local => "local",
            Surface::Score => "score",
        }
    }

    /// The icon the bar shows beside the name.
    pub(crate) fn icon(self) -> Icon {
        match self {
            Surface::Library => Icon::Book,
            Surface::Discover => Icon::Compass,
            Surface::People => Icon::Users,
            Surface::User => Icon::User,
            Surface::Local => Icon::Folder,
            Surface::Score => Icon::Star,
        }
    }
}

/// What the store gave the tab: the detail record, the bodies the tab
/// renders (the README, the tree, the license text), the note, and
/// what each asset of the README is — the cached file, still pending,
/// or not there.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TabData {
    pub detail: Option<yagna_core::model::RepoDetail>,
    pub readme: Option<ReadmeBody>,
    /// Why the README is missing, when its on-demand fetch failed — a
    /// missing README never reads as an empty one.
    pub readme_note: Option<String>,
    pub tree: Option<TreeBody>,
    pub license: Option<String>,
    /// The repository's own note — the reader's sentence about it.
    pub note: Option<String>,
    /// The stored digest, when one was taken — stale or fresh.
    pub digest: Option<DigestBody>,
    /// What each asset id is, keyed by the id.
    pub assets: HashMap<String, Resolved>,
    /// The README's blocks — parsed once, when the data landed. The
    /// render path never parses.
    pub blocks: Vec<Block>,
    /// The structure's shape, likewise parsed once at landing.
    pub tree_nodes: Vec<yagna_core::treefmt::TreeNode>,
    /// The repository's own tags — the reader's names for it.
    pub tags: Vec<String>,
}

/// One open tab: the repository it reads and the view state that
/// belongs to that repository.
#[derive(Debug)]
pub(crate) struct Tab {
    /// The repository the tab reads.
    pub repo: RepoRef,
    /// The catalog's row id for that repository — the key every lazy
    /// layer hangs off.
    pub rowid: i64,
    /// What the bar shows: `owner/name`.
    pub title: String,
    /// The tab's scroll position — remembered across focus changes.
    pub scroll: ScrollHandle,
    /// What the store last gave the tab.
    pub data: Option<TabData>,
    /// The collapsed directories, by their path.
    pub collapsed: HashSet<String>,
    /// Whether the license text is open.
    pub license_open: bool,
    /// Whether the structure drawer is open — closed by default, so a
    /// big repository's tree does not bury its README.
    pub structure_open: bool,
    /// Whether the digest's body is shown — collapsed by default, the
    /// "collapsed full gitingest download" of the pitch.
    pub digest_open: bool,
    /// Whether the repository's images have been asked for — the first
    /// open asks, the later ones do not.
    pub images_started: bool,
    /// Whether the exact first commit has been asked for — one request
    /// per repository, ever.
    pub first_commit_asked: bool,
    /// Whether the missing README has been asked for on demand — once
    /// per tab, so a second open fetches nothing.
    pub readme_asked: bool,
    /// Whether the README's on-demand fetch is in flight — the tab
    /// says "fetching the readme…" while it runs.
    pub readme_fetching: bool,
    /// Why the README's on-demand fetch failed, when it did — kept on
    /// the tab so a re-read of the store does not lose the answer.
    pub readme_error: Option<String>,
    /// Whether the non-standard license text has been asked for — once
    /// per tab, like the README.
    pub license_asked: bool,
    /// What the tab reads when it reads a worktree on this machine —
    /// `None` for a tab over the catalog.
    pub local: Option<LocalTab>,
}

/// What a tab over a worktree holds: the entry it reads, the folders
/// the reader has opened in it, and the file they opened last.
#[derive(Debug)]
pub(crate) struct LocalTab {
    /// The local entry's id — the key the notes hang off.
    pub id: i64,
    /// The entry itself: the path, the branch, the counts.
    pub repo: LocalRepo,
    /// The folders the reader has opened, by their path inside the
    /// worktree. The worktree's root is the empty path.
    pub opened: HashSet<String>,
    /// The folders' entries, read when the reader opened them.
    pub entries: HashMap<String, Vec<LocalEntry>>,
    /// The file the reader opened, and the text the disk gave.
    pub preview: Option<LocalPreview>,
    /// The path of the file the reader opened.
    pub preview_path: Option<String>,
    /// The catalog's row of the same repository, when the origin
    /// matched one — the stars, the health and the topics the service
    /// knows, standing beside the worktree's own facts.
    pub linked: Option<RepoDetail>,
    /// The reader's note about this repository.
    pub note: Option<String>,
}

impl Tab {
    /// A tab over one repository: the identity, the row id, and the
    /// view state a fresh tab starts with.
    fn new(repo: RepoRef, rowid: i64) -> Self {
        let title = format!("{}/{}", repo.owner, repo.name);
        Self {
            repo,
            rowid,
            title,
            scroll: ScrollHandle::new(),
            data: None,
            collapsed: HashSet::new(),
            license_open: false,
            structure_open: false,
            digest_open: false,
            images_started: false,
            first_commit_asked: false,
            readme_asked: false,
            readme_fetching: false,
            readme_error: None,
            license_asked: false,
            local: None,
        }
    }

    /// A tab over a worktree on this machine: the entry it reads, and
    /// the view state a fresh tab starts with.
    fn local(repo: LocalRepo) -> Self {
        let title = repo.name.clone();
        let id = repo.id;
        Self {
            repo: RepoRef {
                forge: yagna_core::model::Forge::GitHub,
                owner: repo.owner.clone().unwrap_or_default(),
                name: repo.repo_name.clone().unwrap_or_else(|| repo.name.clone()),
            },
            rowid: id,
            title,
            scroll: ScrollHandle::new(),
            data: None,
            collapsed: HashSet::new(),
            license_open: false,
            structure_open: false,
            digest_open: false,
            images_started: true,
            first_commit_asked: true,
            readme_asked: true,
            readme_fetching: false,
            readme_error: None,
            license_asked: true,
            local: Some(LocalTab {
                id,
                repo,
                opened: HashSet::new(),
                entries: HashMap::new(),
                preview: None,
                preview_path: None,
                linked: None,
                note: None,
            }),
        }
    }

    /// Whether the tab reads a worktree on this machine.
    pub(crate) fn is_local(&self) -> bool {
        self.local.is_some()
    }
}

/// The open tabs and the focused one.
///
/// The permanent surfaces are always in the bar; the repository tabs
/// follow them. `repo` says which repository tab is in front, when one
/// is; `surface` says which permanent surface the reader is on.
#[derive(Debug, Default)]
pub(crate) struct TabState {
    tabs: Vec<Tab>,
    /// Which permanent surface the reader is on. The surfaces are
    /// never closed, so this is always a real answer.
    surface: Surface,
    /// The repository tab in front, when one is.
    repo: Option<usize>,
}

impl TabState {
    /// The state a fresh window opens with: the library, and no
    /// repository tab.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Opens a local entry's tab and focuses it. A local entry that is
    /// already open is not opened twice — its tab is focused. A local
    /// tab is its own tab: it reads the worktree, not the catalog's
    /// row, even when the two name the same repository.
    pub(crate) fn open_local(&mut self, repo: LocalRepo) -> usize {
        self.repo = None;
        let id = repo.id;
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.local.as_ref().is_some_and(|local| local.id == id))
        {
            self.repo = Some(index);
            return index;
        }
        self.tabs.push(Tab::local(repo));
        let index = self.tabs.len() - 1;
        self.repo = Some(index);
        index
    }

    /// Opens a repository's tab and focuses it. A repository that is
    /// already open is not opened twice — its tab is focused.
    pub(crate) fn open(&mut self, repo: RepoRef, rowid: i64) -> usize {
        self.repo = None;
        // A local tab never answers to a repository's identity: it
        // reads the worktree, and the catalog's row of the same
        // repository is a tab of its own.
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| !tab.is_local() && tab.repo == repo)
        {
            self.repo = Some(index);
            return index;
        }
        self.tabs.push(Tab::new(repo, rowid));
        let index = self.tabs.len() - 1;
        self.repo = Some(index);
        index
    }

    /// Focuses the repository's tab at the index.
    pub(crate) fn focus(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.repo = Some(index);
        }
    }

    /// Puts a permanent surface in front.
    pub(crate) fn focus_surface(&mut self, surface: Surface) {
        self.surface = surface;
        self.repo = None;
    }

    /// The permanent surface in front, when one is.
    pub(crate) fn surface(&self) -> Surface {
        self.surface
    }

    /// Whether the given permanent surface is in front.
    pub(crate) fn surface_in_front(&self, surface: Surface) -> bool {
        self.repo.is_none() && self.surface == surface
    }

    /// Whether a repository tab is in front.
    pub(crate) fn repo_in_front(&self) -> bool {
        self.repo.is_some()
    }

    /// Closes the tab at the index. The focus moves to the next tab,
    /// or to the last one that is left when the closed tab was the
    /// last; a bar with no repository left falls back to the permanent
    /// surface.
    pub(crate) fn close(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        match self.repo {
            Some(focused) if focused == index => {
                self.repo = if self.tabs.is_empty() {
                    None
                } else {
                    Some(index.min(self.tabs.len() - 1))
                };
            }
            // A tab closing to the left shifts the focused one down a
            // slot — the focus stays on the same tab, not the same
            // index.
            Some(focused) if focused > index => self.repo = Some(focused - 1),
            Some(focused) => self.repo = Some(focused.min(self.tabs.len().saturating_sub(1))),
            None => {}
        }
    }

    /// Moves the focus one step through the bar: the permanent
    /// surfaces first, then the repository tabs. Backwards walks the
    /// other way.
    pub(crate) fn cycle(&mut self, forward: bool) {
        let surfaces = Surface::ALL.len();
        let total = surfaces + self.tabs.len();
        if total == 0 {
            return;
        }
        let current = self.position();
        let next = if forward {
            (current + 1) % total
        } else {
            (current + total - 1) % total
        };
        self.focus_position(next);
    }

    /// Where the focus is in the bar's order: the permanent surfaces
    /// first, then the repository tabs.
    fn position(&self) -> usize {
        match self.repo {
            Some(index) => Surface::ALL.len() + index,
            None => Surface::ALL
                .iter()
                .position(|surface| *surface == self.surface)
                .unwrap_or(0),
        }
    }

    /// Puts the focus on the bar's n-th position.
    fn focus_position(&mut self, position: usize) {
        let surfaces = Surface::ALL.len();
        if position < surfaces {
            let surface = Surface::ALL[position];
            self.focus_surface(surface);
            return;
        }
        let index = position - surfaces;
        if index < self.tabs.len() {
            self.repo = Some(index);
        }
    }

    /// The focused repository's tab, when one is in front.
    pub(crate) fn focused_index(&self) -> Option<usize> {
        self.repo
    }

    /// The focused tab.
    pub(crate) fn focused(&self) -> Option<&Tab> {
        self.repo.and_then(|index| self.tabs.get(index))
    }

    /// The focused tab, mutably.
    pub(crate) fn focused_mut(&mut self) -> Option<&mut Tab> {
        self.repo.and_then(|index| self.tabs.get_mut(index))
    }

    /// The tab at the index.
    pub(crate) fn tab(&self, index: usize) -> Option<&Tab> {
        self.tabs.get(index)
    }

    /// The tab at the index, mutably.
    pub(crate) fn tab_mut(&mut self, index: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(index)
    }

    /// The tab of the repository with that row id. A local tab is
    /// never a match: it reads a worktree, not the catalog's row.
    pub(crate) fn index_of(&self, rowid: i64) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| !tab.is_local() && tab.rowid == rowid)
    }

    /// The tab of the local entry with that id.
    pub(crate) fn index_of_local(&self, id: i64) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.local.as_ref().is_some_and(|local| local.id == id))
    }

    /// Every open repository tab.
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
}

/// A tab takes the focus, or closes.
pub(crate) type TabHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A permanent surface takes the focus.
pub(crate) type SurfaceHandler = Rc<dyn Fn(Surface, &mut Window, &mut App)>;

/// What the tab bar's parts report.
#[derive(Clone)]
pub(crate) struct TabActions {
    /// A repository tab was clicked: it takes the focus.
    pub on_focus: TabHandler,
    /// A repository tab was closed: the middle button, or its close button.
    pub on_close: TabHandler,
    /// A permanent surface was clicked.
    pub on_surface: SurfaceHandler,
    /// The window's tooltip: the close button's name.
    pub tip: super::tooltip::TooltipHandle,
}

/// The tab bar: the permanent surfaces first, then one chip per open
/// repository tab. A middle click closes a repository tab; its close
/// button closes it too.
pub(crate) fn render_bar(state: &TabState, actions: &TabActions) -> gpui::Div {
    let mut chips: Vec<gpui::AnyElement> = Surface::ALL
        .iter()
        .map(|surface| render_surface(*surface, state, actions))
        .collect();
    let repository_chips: Vec<gpui::AnyElement> = state
        .tabs()
        .iter()
        .enumerate()
        .map(|(index, tab)| render_chip(index, tab, state.focused_index() == Some(index), actions))
        .collect();
    chips.extend(repository_chips);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(super::chrome_height())
        .px_3()
        .bg(theme::surface())
        .border_b_1()
        .border_color(theme::border())
        .children(chips)
}

/// A permanent surface's chip: its icon, its name, and the neutral
/// pill that marks the one in front.
fn render_surface(surface: Surface, state: &TabState, actions: &TabActions) -> gpui::AnyElement {
    let in_front = state.surface_in_front(surface);
    let on_surface = Rc::clone(&actions.on_surface);
    let ink = if in_front {
        theme::text()
    } else {
        theme::text_muted()
    };
    // The active surface wears its icon's filled shape; the others
    // keep the outline.
    let shape = if in_front {
        filled_icon_element(surface.icon(), ink)
    } else {
        icon_element(surface.icon(), ink)
    };
    div()
        .id(SharedString::from(format!(
            "tab-surface-{}",
            surface.label()
        )))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(if in_front {
            theme::tab_active()
        } else {
            theme::tab_inactive()
        })
        .text_size(RowLayout::small_size())
        .text_color(ink)
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |_, window, cx| on_surface(surface, window, cx))
        .child(shape)
        .child(surface.label())
        .into_any_element()
}

/// One repository tab's chip: its title and its close button.
fn render_chip(index: usize, tab: &Tab, in_front: bool, actions: &TabActions) -> gpui::AnyElement {
    let on_focus = Rc::clone(&actions.on_focus);
    let on_close = Rc::clone(&actions.on_close);
    let closing = Rc::clone(&actions.on_close);
    let title = tab.title.clone();
    let close = actions.tip.clone().attach(
        div()
            .id(SharedString::from(format!("tab-close-{index}")))
            .px_1()
            .rounded_sm()
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                closing(index, window, cx);
            })
            .child("×"),
        &format!("close {title}"),
    );
    div()
        .id(SharedString::from(format!("tab-{index}")))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(if in_front {
            theme::tab_active()
        } else {
            theme::tab_inactive()
        })
        .text_size(RowLayout::small_size())
        .text_color(if in_front {
            theme::text()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |event, window, cx| {
            if is_middle_click(event) {
                on_close(index, window, cx);
                return;
            }
            on_focus(index, window, cx);
        })
        .child(SharedString::from(title))
        .child(close)
        .into_any_element()
}

/// An icon as an element: the shape, painted in the ink beside it.
pub(crate) fn icon_element(icon: Icon, ink: gpui::Rgba) -> gpui::AnyElement {
    gpui::svg()
        .path(icon.asset())
        .size(super::icons::ICON_SIZE)
        .text_color(ink)
        .into_any_element()
}

/// An icon as an element, in its filled shape: the shape the active
/// and the primary states wear. An icon with no filled shape of its own
/// keeps its outline.
pub(crate) fn filled_icon_element(icon: Icon, ink: gpui::Rgba) -> gpui::AnyElement {
    gpui::svg()
        .path(icon.filled_asset().unwrap_or_else(|| icon.asset()))
        .size(super::icons::ICON_SIZE)
        .text_color(ink)
        .into_any_element()
}

/// Whether a click came with the middle mouse button.
fn is_middle_click(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(mouse) => mouse.down.button == MouseButton::Middle,
        ClickEvent::Keyboard(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use yagna_core::model::Forge;

    /// A local entry, as the scan found it.
    fn local_repo(id: i64, name: &str) -> LocalRepo {
        use chrono::TimeZone;
        LocalRepo {
            id,
            root: "C:/repos".to_string(),
            path: format!("C:/repos/{name}"),
            name: name.to_string(),
            owner: Some("someone".to_string()),
            repo_name: Some(name.to_string()),
            origin: Some(format!("https://github.com/someone/{name}.git")),
            branch: Some("main".to_string()),
            last_commit_at: Some(
                Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
                    .single()
                    .expect("date"),
            ),
            file_count: 12,
            total_bytes: 4_096,
            linked_rowid: None,
            missing_at: None,
            scanned_at: Utc
                .with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
                .single()
                .expect("date"),
        }
    }

    fn repo(owner: &str, name: &str) -> RepoRef {
        RepoRef {
            forge: Forge::GitHub,
            owner: owner.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn opening_a_repository_opens_a_tab_and_focuses_it() {
        let mut state = TabState::new();
        assert!(state.surface_in_front(Surface::Library));
        state.open(repo("someone", "thing"), 7);
        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.focused_index(), Some(0));
        assert_eq!(state.focused().map(|tab| tab.rowid), Some(7));
        assert!(!state.surface_in_front(Surface::Library));
    }

    #[test]
    fn opening_an_open_repository_focuses_its_tab() {
        let mut state = TabState::new();
        state.open(repo("someone", "thing"), 7);
        state.open(repo("someone", "other"), 8);
        state.open(repo("someone", "thing"), 7);
        assert_eq!(state.tabs().len(), 2, "the tab is not opened twice");
        assert_eq!(
            state.focused_index(),
            Some(0),
            "the existing tab is focused"
        );
    }

    #[test]
    fn closing_a_tab_moves_the_focus() {
        let mut state = TabState::new();
        state.open(repo("someone", "a"), 1);
        state.open(repo("someone", "b"), 2);
        state.open(repo("someone", "c"), 3);
        state.close(2);
        assert_eq!(state.tabs().len(), 2);
        assert_eq!(state.focused_index(), Some(1), "the focus stays in range");
        state.close(1);
        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.focused_index(), Some(0));
        state.close(0);
        assert!(!state.repo_in_front());
        assert_eq!(state.focused_index(), None);
    }

    #[test]
    fn closing_a_tab_to_the_left_shifts_the_focus_with_it() {
        let mut state = TabState::new();
        for name in ["a", "b", "c", "d", "e"] {
            state.open(repo("someone", name), name.len() as i64);
        }
        assert_eq!(state.focused_index(), Some(4), "the newest tab is on");
        state.close(0);
        assert_eq!(
            state.focused_index(),
            Some(3),
            "the same tab stays in focus; only its index moved"
        );
        state.close(0);
        assert_eq!(state.focused_index(), Some(2));
    }

    #[test]
    fn the_focus_cycles_through_the_bar_and_wraps_around() {
        let mut state = TabState::new();
        state.open(repo("someone", "a"), 1);
        state.open(repo("someone", "b"), 2);
        assert_eq!(state.focused_index(), Some(1));
        state.cycle(true);
        assert_eq!(
            state.focused_index(),
            None,
            "the cycle comes back to the surfaces"
        );
        assert!(state.surface_in_front(Surface::Library));
        state.cycle(true);
        assert!(state.surface_in_front(Surface::Discover));
        state.cycle(true);
        assert!(state.surface_in_front(Surface::People));
        state.cycle(true);
        assert!(state.surface_in_front(Surface::User));
        state.cycle(true);
        assert!(state.surface_in_front(Surface::Local));
        state.cycle(true);
        assert!(state.surface_in_front(Surface::Score));
        state.cycle(true);
        assert_eq!(state.focused_index(), Some(0), "and on into the tabs");
        state.cycle(true);
        assert_eq!(state.focused_index(), Some(1));
        state.cycle(true);
        assert!(
            state.surface_in_front(Surface::Library),
            "the cycle wraps around"
        );
    }

    #[test]
    fn the_cycle_walks_backwards_too() {
        let mut state = TabState::new();
        state.open(repo("someone", "a"), 1);
        assert_eq!(state.focused_index(), Some(0));
        state.cycle(false);
        assert!(
            state.surface_in_front(Surface::Score),
            "back to the last surface"
        );
        state.cycle(false);
        assert!(state.surface_in_front(Surface::Local));
        state.cycle(false);
        assert!(state.surface_in_front(Surface::User));
        state.cycle(false);
        assert!(state.surface_in_front(Surface::People));
    }

    /// A local entry opens its own tab: it reads the worktree, not the
    /// catalog's row — even when the two name the same repository.
    #[test]
    fn a_local_entry_opens_its_own_tab() {
        let mut state = TabState::new();
        let index = state.open_local(local_repo(3, "work"));
        assert_eq!(state.focused_index(), Some(index));
        assert!(state.tabs()[index].is_local());
        assert_eq!(state.tabs()[index].title, "work");

        let again = state.open_local(local_repo(3, "work"));
        assert_eq!(again, index, "the local tab is not opened twice");
        assert_eq!(state.tabs().len(), 1);

        state.open(repo("someone", "work"), 7);
        assert_eq!(state.tabs().len(), 2, "the catalog's row is its own tab");
        assert!(
            state.index_of(7).is_some(),
            "the catalog's row is found by its row id"
        );
        assert!(
            state.index_of_local(3).is_some(),
            "the local entry is found by its own id"
        );
    }

    #[test]
    fn the_library_is_always_there() {
        let mut state = TabState::new();
        assert!(state.surface_in_front(Surface::Library));
        state.open(repo("someone", "a"), 1);
        assert!(!state.surface_in_front(Surface::Library));
        state.close(0);
        assert!(
            state.surface_in_front(Surface::Library),
            "closing the last tab returns the bare list"
        );
    }

    #[test]
    fn the_middle_click_is_the_close_click() {
        let event = ClickEvent::Mouse(gpui::MouseClickEvent {
            down: gpui::MouseDownEvent {
                button: MouseButton::Middle,
                position: gpui::Point::default(),
                ..Default::default()
            },
            up: gpui::MouseUpEvent {
                button: MouseButton::Middle,
                position: gpui::Point::default(),
                ..Default::default()
            },
        });
        assert!(is_middle_click(&event));
        assert!(!is_middle_click(&ClickEvent::default()));
    }
}
