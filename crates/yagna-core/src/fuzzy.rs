//! The fuzzy index: a memory-only projection of the catalog that ranks
//! repositories against a typed query.
//!
//! The index is derived, never stored: it is built from one read of the
//! catalog at startup and thrown away at exit. The SQL catalog stays the
//! single source of truth; this is a disposable lens over it, rebuilt by
//! reconstruction and patched in place when a tag changes.
//!
//! The matcher is generous: it matches any subsequence, so a query of
//! six consonants lands somewhere in every long description in the
//! catalog. The ranking stays nucleo's; the *gate* is this module's —
//! a match has to be close: within a few edits of some window of the
//! text. A typo is still a match; a coincidence is not.
//!
//! Items: `SearchItem` — one indexed repository (its row id and its
//! haystack); `SearchHit` — one match (its row id and its score);
//! `FuzzyIndex` — the index itself (`build`, `patch`, `search`,
//! `matches`); `haystack` — the text one repository is matched against;
//! `is_close_match` — the gate.
//! Core functions: `haystack` — builds the matched text from a
//! repository's fields; `is_close_match` — is this a real match;
//! `FuzzyIndex::search` — the ranked row ids.
//! Depends on: nucleo (the matcher), yagna_core's own types.
//! Called by: the app's list pane (the search box) and the store's
//! callers that assemble the index.

use std::collections::HashMap;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

/// One indexed repository: the catalog's row id and the text the matcher
/// scores against.
#[derive(Debug, Clone)]
pub struct SearchItem {
    pub repo_rowid: i64,
    pub haystack: Utf32String,
}

impl SearchItem {
    /// One indexed repository from a row id and its haystack text.
    pub fn new(repo_rowid: i64, haystack: &str) -> Self {
        Self {
            repo_rowid,
            haystack: Utf32String::from(haystack),
        }
    }
}

/// The text one repository is matched against: the name, the owner, the
/// topics, the user's tags and the description, space-separated. The
/// order is the tie-break order for a match that spans several fields —
/// the name is the strongest signal, so it comes first.
pub fn haystack(
    owner: &str,
    name: &str,
    description: Option<&str>,
    topics: &[String],
    tags: &[String],
) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(2 + topics.len() + tags.len() + 1);
    parts.push(name);
    parts.push(owner);
    parts.extend(topics.iter().map(String::as_str));
    parts.extend(tags.iter().map(String::as_str));
    if let Some(description) = description {
        parts.push(description);
    }
    parts.join(" ")
}

/// Whether the query is close to some window of the text — within a
/// few edits, the way a typo is.
///
/// This is the gate the generous matcher needs. A subsequence spread
/// over a paragraph is a coincidence, not a match; a transposition or a
/// dropped character is a match, and the reader expects it to land.
/// The tolerance is a third of the query's length, at least one edit.
pub fn is_close_match(query: &str, haystack: &str) -> bool {
    is_close_chars(query, haystack.chars())
}

/// The gate over a text the matcher already holds in its own string
/// type: the characters come in as they come.
///
/// The query folds to lowercase the same way the haystack's characters
/// do below — nucleo's smart case passes an all-caps query through
/// case-sensitively, and the gate must not then reject the exact name
/// it just matched.
fn is_close_chars(query: &str, haystack: impl Iterator<Item = char>) -> bool {
    let query_chars: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if query_chars.is_empty() {
        return false;
    }
    let tolerance = edit_tolerance(query_chars.len());
    let distance = closest_window_distance(&query_chars, haystack);
    distance.is_some_and(|distance| distance <= tolerance)
}

/// How many edits a match of that length may differ by. The
/// tolerance grows slowly, so a longer query is never the more
/// forgiving one: "match" and "matcha" must not disagree about what
/// counts as a match.
fn edit_tolerance(length: usize) -> usize {
    (length / 4).max(1)
}

/// The smallest number of edits between the query and any window of the
/// text — the classic approximate-string-matching row, where the first
/// row is all zeros so a match may start anywhere.
///
/// A word boundary resets the row: the window has to live inside one
/// word. A query matches a *word* of the repository — its name, its
/// owner, a topic, a tag, a word of the description — and a word is
/// short enough that a coincidence is rare.
fn closest_window_distance(query: &[char], haystack: impl Iterator<Item = char>) -> Option<usize> {
    // The row starts as the cost of an empty text: matching the
    // query's first n characters against nothing costs n insertions.
    let mut row: Vec<usize> = (0..=query.len()).collect();
    let mut best: Option<usize> = None;
    for character in haystack {
        if character.is_whitespace() {
            reset_row(&mut row);
            continue;
        }
        relax_row(query, &mut row, character.to_ascii_lowercase());
        let distance = row[query.len()];
        best = Some(best.map_or(distance, |current: usize| current.min(distance)));
    }
    best
}

/// The row after a word boundary: a window may not cross one.
fn reset_row(row: &mut [usize]) {
    for (index, cell) in row.iter_mut().enumerate().skip(1) {
        *cell = index;
    }
}

/// One haystack character's edit-distance row: the classic
/// approximate-string-matching step, where row[0] stays zero so a
/// window may start anywhere.
fn relax_row(query: &[char], row: &mut [usize], character: char) {
    let mut previous_diagonal = row[0];
    row[0] = 0;
    for index in 1..=query.len() {
        let previous = row[index];
        let cost = usize::from(query[index - 1] != character);
        row[index] = (previous + 1)
            .min(row[index - 1] + 1)
            .min(previous_diagonal + cost);
        previous_diagonal = previous;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHit {
    pub repo_rowid: i64,
    pub score: u32,
}

/// The fuzzy index: the haystacks, the row ids they belong to, and the
/// position of each row in the haystack list (so a tag edit patches one
/// row instead of rebuilding the whole index).
#[derive(Debug, Clone, Default)]
pub struct FuzzyIndex {
    items: Vec<SearchItem>,
    positions: HashMap<i64, usize>,
}

impl FuzzyIndex {
    /// The index's items — the haystacks, for a measurement.
    pub fn items(&self) -> &[SearchItem] {
        &self.items
    }

    /// Builds the index from the catalog's search rows.
    pub fn build(items: Vec<SearchItem>) -> Self {
        let positions = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.repo_rowid, index))
            .collect();
        Self { items, positions }
    }

    /// How many repositories the index holds.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Patches one row's haystack in place — the tag-edit path. A row
    /// that is not in the index is ignored: the index is a projection,
    /// and the next full rebuild is the truth.
    pub fn patch(&mut self, repo_rowid: i64, haystack: &str) {
        let Some(position) = self.positions.get(&repo_rowid).copied() else {
            return;
        };
        self.items[position].haystack = Utf32String::from(haystack);
    }

    /// Whether the query matches the repository with that row id.
    pub fn matches(&self, query: &str, repo_rowid: i64) -> bool {
        self.search(query)
            .iter()
            .any(|hit| hit.repo_rowid == repo_rowid)
    }

    /// The ranked matches for a query: the hits, best score first. An
    /// empty query matches nothing — the caller treats "no query" as
    /// "no filter", not as "everything matches".
    ///
    /// The matcher is generous, so every hit is put through the
    /// closeness gate: the query has to be within a few edits of some
    /// window of the haystack. Without it, six consonants match every
    /// long description in the catalog.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut scored: Vec<SearchHit> = Vec::new();
        for item in &self.items {
            let score = pattern.score(item.haystack.slice(..), &mut matcher);
            let Some(score) = score else {
                continue;
            };
            if !is_close_chars(query, item.haystack.slice(..).chars()) {
                continue;
            }
            scored.push(SearchHit {
                repo_rowid: item.repo_rowid,
                score,
            });
        }
        // Best score first; the row id breaks a tie, so the order never
        // depends on the hash map's iteration order.
        scored.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.repo_rowid.cmp(&right.repo_rowid))
        });
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture mirrors the catalog's own hard class: two
    /// repositories whose names differ by one letter (`zedis` and
    /// `zliksl`), the editor they both resemble, and one repository
    /// with nothing to do with them.
    fn rows() -> Vec<(i64, String)> {
        let rows = [
            ("zedis", "a fast key-value store", "rust storage"),
            ("zliksl", "the zed editor", "rust editor"),
            ("zed", "the zed editor", "rust editor"),
            ("ripgrep", "search recursively", "rust cli"),
        ];
        rows.iter()
            .enumerate()
            .map(|(index, (name, description, topics))| {
                let topics = topics.split(' ').map(str::to_string).collect::<Vec<_>>();
                (
                    index as i64 + 1,
                    haystack("owner", name, Some(description), &topics, &[]),
                )
            })
            .collect()
    }

    fn index() -> FuzzyIndex {
        FuzzyIndex::build(
            rows()
                .iter()
                .map(|(rowid, text)| SearchItem::new(*rowid, text))
                .collect(),
        )
    }

    #[test]
    fn haystack_carries_every_field() {
        let topics = vec!["cli".to_string()];
        let tags = vec!["reading list".to_string()];
        let text = haystack("someone", "something", Some("a repo"), &topics, &tags);
        assert!(text.contains("someone"));
        assert!(text.contains("something"));
        assert!(text.contains("a repo"));
        assert!(text.contains("cli"));
        assert!(text.contains("reading list"));
    }

    #[test]
    fn a_typo_still_matches_the_real_name() {
        let index = index();
        let matched: Vec<i64> = index
            .search("zliksl")
            .into_iter()
            .map(|hit| hit.repo_rowid)
            .collect();
        assert!(matched.contains(&2), "the exact row matches: {matched:?}");
        assert!(
            index.matches("zedis", 1),
            "the other name matches its own spelling"
        );
        assert!(
            index.matches("edis", 1),
            "a dropped character still lands on the row"
        );
        assert!(
            index.matches("zedos", 1),
            "a substituted character still lands on the row"
        );
    }

    #[test]
    fn the_gate_keeps_a_typo_and_cuts_a_coincidence() {
        assert!(
            is_close_match("zliksl", "zliksl a fast key-value store"),
            "the exact name matches"
        );
        assert!(
            is_close_match("edis", "zedis a fast key-value store"),
            "a dropped character still lands"
        );
        assert!(
            is_close_match("zedos", "zedis a fast key-value store"),
            "a substituted character still lands"
        );
        assert!(
            !is_close_match(
                "kbhkin",
                "lukasbach react-complex-tree react component tree accessible"
            ),
            "six consonants are not a match"
        );
        assert!(
            !is_close_match("kbhkin", "Smaug6739 markdown note-taking knowledge-base"),
            "nor in prose"
        );
    }

    #[test]
    fn a_random_string_matches_nothing() {
        let index = index();
        assert!(
            index.search("kbhkin").is_empty(),
            "a string of consonants matches nothing"
        );
        assert!(
            index.search("qqqqqqqq").is_empty(),
            "and neither does a string of one letter"
        );
    }

    #[test]
    fn a_query_that_matches_nothing_matches_nothing() {
        let index = index();
        assert!(index.search("qqqqqqqq").is_empty());
    }

    #[test]
    fn an_exact_all_caps_name_matches_its_own_query() {
        let index = FuzzyIndex::build(vec![SearchItem::new(
            1,
            "SDL the cross-platform development library",
        )]);
        assert!(
            index.matches("SDL", 1),
            "the gate folds the query like it folds the haystack"
        );
        assert!(index.matches("sdl", 1));
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let index = index();
        assert!(index.search("").is_empty());
        assert!(index.search("   ").is_empty());
    }

    #[test]
    fn the_best_score_comes_first() {
        let index = FuzzyIndex::build(vec![
            SearchItem::new(1, "owner averyveryverylongname something else"),
            SearchItem::new(2, "owner zed"),
        ]);
        let matched = index.search("zed");
        assert_eq!(
            matched.first().map(|hit| hit.repo_rowid),
            Some(2),
            "the exact name outranks the buried mention: {matched:?}"
        );
    }

    #[test]
    fn patching_one_row_changes_only_that_row() {
        let mut index = index();
        index.patch(1, "owner an entirely different repository");
        assert!(
            !index.matches("key-value", 1),
            "the patched row no longer matches its old text"
        );
        index.patch(1, "owner zedis a fast key-value store rust storage");
        assert!(
            index.matches("key-value", 1),
            "the patched row matches its new text"
        );
        assert_eq!(index.len(), 4, "the patch did not grow the index");
    }

    #[test]
    fn patching_an_unknown_row_is_a_no_op() {
        let mut index = index();
        index.patch(999, "nothing");
        assert_eq!(index.len(), 4);
    }

    #[test]
    fn the_index_reports_its_emptiness() {
        let index = FuzzyIndex::default();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }
}
