//! The expansion state: which rows are open, what their preview holds,
//! and the generation the chevron's rotation reads.
//!
//! The expansion is per session: the previews are memory-only, and a
//! restart folds every row — unless the user turned the persistence
//! setting on, in which case the open set survives in the store (the
//! pane saves it on every toggle and restores it at launch).
//! The preview is fetched once per row and kept while the row stays
//! open — the store is the source of truth, the preview is a copy.
//!
//! The README is parsed when the preview lands, not while the row is
//! painted: the row is drawn on every frame, and the parse is CPU
//! work. What the row paints is the README *rendered* — the badges
//! collapsed into a count, never the raw markdown source.
//!
//! Items: `RowPreview` — what the store handed over: the README head
//! and the row's image; `PreviewView` — what the row paints: the
//! rendered README, the badge count, the image; `ExpandedState` — the
//! open set, the per-row animation generation, the fetched previews,
//! the rows being fetched.
//! Core functions: `toggle` — opens or closes a row and bumps its
//! generation; `begin`/`finish` — the fetch bookkeeping; `open_ids`/
//! `restore` — the persisted set's two ends.
//! Depends on: yagna_store (the preview's records), yagna_core (the
//! README's rewrite, the renderer), crate::ui::readme (the preview's
//! resolver).
//! Called by: crate::ui (the list pane and the row renderer).

use std::collections::{HashMap, HashSet};

use yagna_core::markdown::{RawBase, rewrite_assets};
use yagna_core::mdrender::Block;
use yagna_store::{AssetRecord, ReadmeBody};

use super::readme::PreviewResolver;

/// What an open row shows: the README's rendered head and the image the
/// row previews. Either may be absent — a repository without a README
/// has no head, a repository without an image has no asset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowPreview {
    /// The README's rewritten body: its image destinations are the
    /// asset ids the store minted.
    pub head: Option<ReadmeBody>,
    /// The row's whole asset set, so the preview's answer book covers
    /// every image the head mentions.
    pub assets: Vec<AssetRecord>,
    /// The row's own image.
    pub asset: Option<AssetRecord>,
}

/// What an open row paints: the README rendered, the badges collapsed
/// into a count, and the row's own image.
#[derive(Debug)]
pub(crate) struct PreviewView {
    /// The rendered README: the blocks the row paints.
    pub(crate) blocks: Vec<Block>,
    /// How many badges the count line announces.
    pub(crate) badges: usize,
    /// The row's own image: the store's record.
    pub(crate) asset: Option<AssetRecord>,
    /// How many lines the README has in total.
    pub(crate) total_lines: usize,
    /// Whether the README is longer than the head the row holds.
    pub(crate) truncated: bool,
}

impl PreviewView {
    /// The view of a fetched preview: the README parsed, the badges
    /// collapsed into a count, the row's own image kept.
    fn parse(preview: RowPreview) -> Self {
        let Some(head) = preview.head.as_ref() else {
            return Self::empty(preview.asset);
        };
        let (rewritten, entries) = rewrite_assets(&preview_text(head), &preview_base());
        let resolver = PreviewResolver::new(&entries, &preview.assets, preview.asset.as_ref());
        let rendered = yagna_core::mdrender::preview(&rewritten, &resolver);
        Self {
            blocks: rendered.blocks,
            badges: rendered.badges,
            asset: preview.asset,
            total_lines: preview_text(head).lines().count(),
            truncated: head.truncated,
        }
    }

    /// The view of a row whose README was never fetched: no blocks, no
    /// badges, and the image the row still has.
    fn empty(asset: Option<AssetRecord>) -> Self {
        Self {
            blocks: Vec::new(),
            badges: 0,
            asset,
            total_lines: 0,
            truncated: false,
        }
    }
}

/// The head of the README the row shows: the first lines of the
/// stored body. The line budget keeps the parse cheap — a row is
/// drawn on every frame, and the head is what the reader sees.
fn preview_text(head: &ReadmeBody) -> String {
    head.rewritten
        .lines()
        .take(super::PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The raw base the preview's rewrite resolves against. The row's view
/// model carries no repository reference, so a relative image
/// destination is given an id the store never minted — that image
/// shows the placeholder. The badges and the absolute images are
/// unaffected: their ids are their URLs' hashes, and the badge flag
/// comes from the URL.
fn preview_base() -> RawBase {
    RawBase {
        owner: String::new(),
        repo: String::new(),
        branch: "HEAD".to_string(),
    }
}

/// The expansion state of the list: which rows are open, what their
/// previews hold, and the animation generation each chevron reads.
#[derive(Debug, Default)]
pub(crate) struct ExpandedState {
    open: HashSet<i64>,
    generations: HashMap<i64, u32>,
    previews: HashMap<i64, PreviewView>,
    pending: HashSet<i64>,
}

impl ExpandedState {
    /// Whether the row is open.
    pub(crate) fn is_open(&self, repo_rowid: i64) -> bool {
        self.open.contains(&repo_rowid)
    }

    /// Opens or closes a row and returns its new state. Every toggle
    /// bumps the row's generation: the chevron's rotation reads it, and
    /// a new generation is what makes the animation run again.
    pub(crate) fn toggle(&mut self, repo_rowid: i64) -> bool {
        let now_open = if self.open.contains(&repo_rowid) {
            self.open.remove(&repo_rowid);
            false
        } else {
            self.open.insert(repo_rowid);
            true
        };
        let generation = self.generations.entry(repo_rowid).or_insert(0);
        *generation += 1;
        now_open
    }

    /// The row's animation generation: zero until the row is toggled
    /// for the first time.
    pub(crate) fn generation(&self, repo_rowid: i64) -> u32 {
        self.generations.get(&repo_rowid).copied().unwrap_or(0)
    }

    /// The row's preview, once it has been fetched: the README
    /// rendered and the row's own image.
    pub(crate) fn preview(&self, repo_rowid: i64) -> Option<&PreviewView> {
        self.previews.get(&repo_rowid)
    }

    /// The asset ids the open rows' previews are rendering — the pin
    /// set the cache eviction honors: a view's image is never evicted
    /// under it.
    pub(crate) fn pinned_asset_ids(&self) -> Vec<String> {
        self.previews
            .values()
            .filter_map(|preview| preview.asset.as_ref())
            .map(|asset| asset.asset_id.clone())
            .collect()
    }

    /// Lands a fetched preview: the README is parsed here, once, and
    /// the row paints the blocks from then on.
    pub(crate) fn set_preview(&mut self, repo_rowid: i64, preview: RowPreview) {
        self.previews
            .insert(repo_rowid, PreviewView::parse(preview));
    }

    /// Marks a row as being fetched; returns false when it already is
    /// (or already has its preview), so the fetch happens once.
    pub(crate) fn begin(&mut self, repo_rowid: i64) -> bool {
        if self.previews.contains_key(&repo_rowid) || self.pending.contains(&repo_rowid) {
            return false;
        }
        self.pending.insert(repo_rowid);
        true
    }

    /// Marks a row's fetch as finished, with or without a preview.
    pub(crate) fn finish(&mut self, repo_rowid: i64) {
        self.pending.remove(&repo_rowid);
    }

    /// Forgets a row's preview — the on-demand README's way to make
    /// the row ask for itself again once the store holds the body.
    pub(crate) fn forget(&mut self, repo_rowid: i64) {
        self.previews.remove(&repo_rowid);
    }

    /// Whether the row's preview is being fetched right now.
    pub(crate) fn is_pending(&self, repo_rowid: i64) -> bool {
        self.pending.contains(&repo_rowid)
    }

    /// How many rows are open.
    pub(crate) fn open_count(&self) -> usize {
        self.open.len()
    }

    /// The open rows' ids, sorted — the JSON text the store persists.
    pub(crate) fn open_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.open.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Restores a persisted set: the rows open again, without touching
    /// generations or previews.
    pub(crate) fn restore(&mut self, ids: &[i64]) {
        self.open = ids.iter().copied().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toggle_opens_then_closes_the_row() {
        let mut state = ExpandedState::default();
        assert!(!state.is_open(7));
        assert!(state.toggle(7), "the first toggle opens");
        assert!(state.is_open(7));
        assert!(!state.toggle(7), "the second toggle closes");
        assert!(!state.is_open(7));
    }

    #[test]
    fn every_toggle_bumps_the_generation() {
        let mut state = ExpandedState::default();
        assert_eq!(state.generation(7), 0, "an untouched row has no rotation");
        state.toggle(7);
        assert_eq!(state.generation(7), 1);
        state.toggle(7);
        assert_eq!(state.generation(7), 2);
        assert_eq!(state.generation(8), 0, "another row is untouched");
    }

    #[test]
    fn a_row_is_fetched_once() {
        let mut state = ExpandedState::default();
        assert!(state.begin(7), "the first begin fetches");
        assert!(!state.begin(7), "the second begin waits");
        assert!(state.is_pending(7));
        state.finish(7);
        assert!(!state.is_pending(7));
        assert!(state.begin(7), "no preview yet, so it fetches again");
    }

    #[test]
    fn a_loaded_preview_is_not_fetched_again() {
        let mut state = ExpandedState::default();
        state.set_preview(7, RowPreview::default());
        assert!(!state.begin(7), "the preview is already there");
    }

    #[test]
    fn the_open_rows_are_counted() {
        let mut state = ExpandedState::default();
        state.toggle(1);
        state.toggle(2);
        assert_eq!(state.open_count(), 2);
    }

    /// The persisted set's two ends: the ids read back sorted, and a
    /// restored set opens without touching generations or previews.
    #[test]
    fn the_open_set_round_trips_through_ids() {
        let mut state = ExpandedState::default();
        state.toggle(9);
        state.toggle(2);
        assert_eq!(state.open_ids(), vec![2, 9]);
        let mut restored = ExpandedState::default();
        restored.restore(&[9, 2]);
        assert!(restored.is_open(2) && restored.is_open(9));
        assert_eq!(restored.generation(2), 0, "a restored row has no rotation");
        assert_eq!(restored.open_count(), 2);
        restored.restore(&[]);
        assert_eq!(restored.open_count(), 0);
    }

    /// The preview holds the README rendered, never the raw source: the
    /// badge wall is counted, and the link syntax never survives.
    #[test]
    fn the_preview_holds_the_readme_rendered() {
        let head = ReadmeBody {
            rewritten: "[![build](https://img.shields.io/badge/build-passing-brightgreen)](https://example.org/ci)\n\nSee [the guide](https://example.org/guide).\n".to_string(),
            truncated: false,
        };
        let mut state = ExpandedState::default();
        state.set_preview(
            7,
            RowPreview {
                head: Some(head),
                assets: Vec::new(),
                asset: None,
            },
        );
        let preview = state.preview(7).expect("a preview");
        assert_eq!(preview.badges, 1, "the badge wall is counted");
        let visible = yagna_core::mdrender::visible_text(&preview.blocks);
        assert!(
            !visible.contains("]("),
            "the raw link syntax never reaches the reader: {visible}"
        );
        assert!(
            visible.contains("the guide"),
            "the link shows its own text: {visible}"
        );
    }
}
