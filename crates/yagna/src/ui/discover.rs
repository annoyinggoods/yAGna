//! The DISCOVER tab: the search API's one query shape over catalog-style
//! rows, rendered over in-memory snapshots and the stored shadows.
//!
//! One query shape, five filters, no pagination beyond a repeat query
//! — the OhNiceRepo ceiling on purpose; the word and the topic joined
//! the days, the stars and the language as the amendment. One query
//! per interaction, debounced at the search budget's floor; the tab
//! shows the remaining budget beside the search action when the probe
//! knows it. Starring is deliberately absent: it happens on GitHub.
//!
//! The five controls are labelled — "created within", "minimum stars",
//! "language", "topic (one)" and "word" — and the counted ones name
//! their unit; the topic is one because GitHub's search takes one
//! `topic:` qualifier per query, and the label says so. The sort is
//! the one dropdown component: a click opens the two orders, a pick
//! writes the choice. A search that matched nothing names the query
//! that matched nothing; a failed one says so in the alarm ink, and
//! the last results stay standing.
//!
//! Items: `DiscoverState` — the query, its results and its budget;
//! `Control` — one labelled control; `DiscoverFields` — the five
//! fields the pane lends the row; `sort_options`/`sort_at` — the
//! sort picker's list; `counter_line` — the budget line;
//! `empty_frame`/`error_frame` — the framed states.
//! Depends on: yagna_core (the query shape, the word's quoting),
//! yagna_forge (the debounce floor), gpui (the elements),
//! crate::ui::dropdown (the sort picker), crate::ui::theme.
//! Called by: crate::ui (the list pane, DISCOVER tab in front).

use std::rc::Rc;
use std::time::Instant;

use gpui::{App, ClickEvent, KeyDownEvent, Rgba, SharedString, Window, div, prelude::*, px};
use yagna_core::model::{DiscoverQuery, DiscoverSort, RepoSnapshot};
use yagna_forge::SEARCH_DEBOUNCE_SECS;

use super::dropdown::{DropdownOption, Site};
use super::field::TextField;
use super::rows::RowLayout;
use super::theme;

/// The search endpoint's budget: how many queries a minute it allows.
const SEARCH_RATE_PER_MINUTE: u32 = 30;

/// The DISCOVER tab's state: the query the controls hold, the results
/// of the last run, and the search budget the probe last saw.
#[derive(Default)]
pub(crate) struct DiscoverState {
    /// The query the controls hold.
    pub query: DiscoverQuery,
    /// The last run's results, most stars first.
    pub results: Vec<RepoSnapshot>,
    /// Whether a search is in flight.
    pub running: bool,
    /// What the last run said, when it said anything.
    pub note: Option<String>,
    /// The search budget the probe last saw.
    pub remaining: Option<u64>,
    /// When the last run started — the debounce clock.
    pub last_run: Option<Instant>,
    /// What the last run failed with, when it failed.
    pub error: Option<String>,
    /// Whether a search has run at all: the empty frame's cue.
    pub searched: bool,
}

impl DiscoverState {
    /// Whether a run may start now: one query per interaction, at most
    /// one per debounce window.
    pub(crate) fn can_run(&self) -> bool {
        if self.running {
            return false;
        }
        match self.last_run {
            Some(started) => started.elapsed().as_secs() >= SEARCH_DEBOUNCE_SECS,
            None => true,
        }
    }

    /// The budget line beside the controls: said when the probe knows
    /// the budget, quiet otherwise.
    pub(crate) fn counter_line(&self) -> Option<String> {
        self.remaining
            .map(|left| format!("search: {left} queries left this minute"))
    }

    /// The query the state holds, in the search API's spelling: the
    /// day count, the star floor, the language, the topic and the
    /// quoted word.
    pub(crate) fn query_label(&self) -> String {
        let mut label = format!(
            "created:>{} stars:>{}",
            self.query.created_within_days, self.query.min_stars
        );
        if let Some(language) = trimmed(&self.query.language) {
            label.push_str(&format!(" language:{language}"));
        }
        if let Some(topic) = trimmed(&self.query.topic) {
            label.push_str(&format!(" topic:{topic}"));
        }
        if let Some(word) = trimmed(&self.query.word) {
            label.push(' ');
            label.push_str(&yagna_core::model::quote_word(word));
        }
        label
    }

    /// Records a failed run: the problem rides along, the quiet note
    /// goes, and the last results stay standing.
    pub(crate) fn record_failure(&mut self, problem: &str) {
        self.error = Some(problem.to_string());
        self.note = None;
        self.searched = true;
    }

    /// Records a run that landed: the results stand and the loud frame
    /// is gone.
    pub(crate) fn record_success(&mut self) {
        self.error = None;
        self.searched = true;
    }

    /// Whether the last run matched nothing: the empty frame's cue.
    /// A run that failed is not an empty result — the loud frame says
    /// so instead.
    pub(crate) fn matched_nothing(&self) -> bool {
        self.searched && self.results.is_empty() && self.error.is_none() && !self.running
    }

    /// The order the query asks for, and the way to change it: the
    /// picker's two orders, by index.
    pub(crate) fn set_sort(&mut self, sort: DiscoverSort) {
        self.query.sort = sort;
    }

    /// The label the sort control wears: the order the query asks for.
    pub(crate) fn sort_label(&self) -> &'static str {
        self.query.sort.label()
    }

    /// Puts the filters back the way they came: the default query, no
    /// results, no error. The clear-filters action.
    pub(crate) fn clear_filters(&mut self) {
        self.query = DiscoverQuery::default();
        self.results.clear();
        self.error = None;
        self.note = None;
        self.searched = false;
    }
}

/// One labelled control: the label it wears, the unit its value counts
/// in — none for a value that has no unit — and the width its field
/// takes.
pub(crate) struct Control {
    /// The label the control wears.
    pub label: &'static str,
    /// The unit the value counts in, when it has one.
    pub unit: Option<Unit>,
    /// The width the field takes.
    pub width: gpui::Pixels,
}

/// A unit with two forms: one day, many days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unit {
    /// The unit of a single value.
    pub one: &'static str,
    /// The unit of several values.
    pub many: &'static str,
}

impl Unit {
    /// The unit as the value reads it: one for a single value, many
    /// otherwise.
    pub(crate) fn word(&self, value: u32) -> &'static str {
        if value == 1 { self.one } else { self.many }
    }
}

/// The days control: "created within", counted in days.
pub(crate) const DAYS: Control = Control {
    label: "created within",
    unit: Some(Unit {
        one: "day",
        many: "days",
    }),
    width: px(90.0),
};

/// The stars control: "minimum stars", counted in stars.
pub(crate) const STARS: Control = Control {
    label: "minimum stars",
    unit: Some(Unit {
        one: "star",
        many: "stars",
    }),
    width: px(110.0),
};

/// The language control: "language", with no unit.
pub(crate) const LANGUAGE: Control = Control {
    label: "language",
    unit: None,
    width: px(160.0),
};

/// The topic control: "topic (one)" — GitHub's search takes one
/// `topic:` qualifier per query, and the label says so rather than
/// pretending the reader could ask for two.
pub(crate) const TOPIC: Control = Control {
    label: "topic (one)",
    unit: None,
    width: px(160.0),
};

/// The word control: "word", with no unit — the quoting of what the
/// reader types is the core's rule, not the field's.
pub(crate) const WORD: Control = Control {
    label: "word",
    unit: None,
    width: px(160.0),
};

/// The sort control: "sort", with no unit — the order is not a
/// quantity.
pub(crate) const SORT: Control = Control {
    label: "sort",
    unit: None,
    width: px(120.0),
};

/// A control's keystroke handler: the field's keystrokes, handed on
/// to the pane.
pub(crate) type KeyHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

/// The five controls' key handlers: the keystrokes each field
/// reports to the pane.
pub(crate) struct ControlKeys {
    /// The days field's keystrokes.
    pub days: KeyHandler,
    /// The stars field's keystrokes.
    pub stars: KeyHandler,
    /// The language field's keystrokes.
    pub language: KeyHandler,
    /// The topic field's keystrokes.
    pub topic: KeyHandler,
    /// The word field's keystrokes.
    pub word: KeyHandler,
}

/// One optional filter's text: trimmed, blank riding as absent.
fn trimmed(text: &Option<String>) -> Option<&str> {
    text.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

/// One labelled control: the label, the field, and the unit the value
/// counts in. The value is the state's — the pane's fields are cleared
/// after every commit, so the state is the truth. A control without a
/// unit ignores its value.
pub(crate) fn render_control(
    control: &Control,
    field: &TextField,
    value: u32,
    window: &mut Window,
    on_key: &KeyHandler,
) -> gpui::Stateful<gpui::Div> {
    let handler = Rc::clone(on_key);
    let focus = field.focus().clone();
    let element = field.render(
        window,
        move |event, window, cx| handler(event, window, cx),
        move |window, _cx| window.focus(&focus),
    );
    let mut row = div()
        .id(SharedString::from(format!(
            "discover-control-{}",
            control.label
        )))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(control.label)),
        )
        .child(element.w(control.width));
    row = row.when_some(control.unit.map(|unit| unit.word(value)), |row, word| {
        row.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(word)),
        )
    });
    row
}

/// The five controls' text fields, as the pane holds them: one ref
/// per filter, bundled so the render call stays small.
pub(crate) struct DiscoverFields<'a> {
    /// The days field.
    pub days: &'a TextField,
    /// The stars field.
    pub stars: &'a TextField,
    /// The language field.
    pub language: &'a TextField,
    /// The topic field.
    pub topic: &'a TextField,
    /// The word field.
    pub word: &'a TextField,
}

/// The five controls as one row: the days, the stars, the language,
/// the topic and the word, each labelled, with its own field — the
/// counted ones carry their unit.
pub(crate) fn render_controls(
    state: &DiscoverState,
    fields: &DiscoverFields,
    window: &mut Window,
    keys: &ControlKeys,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(render_control(
            &DAYS,
            fields.days,
            state.query.created_within_days,
            window,
            &keys.days,
        ))
        .child(render_control(
            &STARS,
            fields.stars,
            state.query.min_stars,
            window,
            &keys.stars,
        ))
        .child(render_control(
            &LANGUAGE,
            fields.language,
            0,
            window,
            &keys.language,
        ))
        .child(render_control(&TOPIC, fields.topic, 0, window, &keys.topic))
        .child(render_control(&WORD, fields.word, 0, window, &keys.word))
}

/// The hint under the controls: what a search costs and how often one
/// may run. The wording lives here, and nowhere else.
pub(crate) fn hint_line() -> String {
    format!(
        "each search costs one of the {SEARCH_RATE_PER_MINUTE} requests per minute the search endpoint allows · one search every {SEARCH_DEBOUNCE_SECS} seconds"
    )
}

/// The hint under the controls, in the quiet ink.
pub(crate) fn render_hint() -> gpui::Div {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(hint_line()))
}

/// The sort picker's options: the two orders the query can ask for,
/// the chosen one marked.
pub(crate) fn sort_options(state: &DiscoverState) -> Vec<DropdownOption> {
    [DiscoverSort::Stars, DiscoverSort::Newest]
        .iter()
        .map(|sort| DropdownOption::new(sort.label(), *sort == state.query.sort))
        .collect()
}

/// The sort the picker's index names.
pub(crate) fn sort_at(index: usize) -> Option<DiscoverSort> {
    [DiscoverSort::Stars, DiscoverSort::Newest]
        .into_iter()
        .nth(index)
}

/// The sort control: the label it wears, and the picker beside it —
/// the one dropdown component, standing where the cycle button stood.
pub(crate) fn render_sort(
    state: &DiscoverState,
    open: bool,
    highlight: usize,
    focus: &gpui::FocusHandle,
    actions: &super::dropdown::DropdownActions,
) -> gpui::AnyElement {
    let picker = super::dropdown::render_inline(
        Site::DiscoverSort,
        state.sort_label(),
        open,
        highlight,
        &sort_options(state),
        focus,
        actions,
    );
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(SORT.label)),
        )
        .child(picker)
        .into_any_element()
}

/// The budget line beside the search action: the queries the minute
/// has left, said when the probe knows them.
pub(crate) fn render_budget(state: &DiscoverState) -> Option<gpui::Div> {
    let line = state.counter_line()?;
    Some(
        div()
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .child(SharedString::from(line)),
    )
}

/// The empty frame's text: the query that matched nothing.
pub(crate) fn empty_frame(query: &str) -> String {
    format!("no matches for \"{query}\"")
}

/// The idle frame's text: what the surface does before the first
/// search — the reader is never looking at a blank canvas.
pub(crate) fn idle_frame() -> String {
    format!(
        "no search yet — set the filters and press search; each search costs one of the {SEARCH_RATE_PER_MINUTE} requests per minute"
    )
}

/// The idle frame: a bordered box that says the surface is waiting.
pub(crate) fn render_idle_frame() -> gpui::Div {
    frame(theme::text_muted(), theme::border()).child(SharedString::from(idle_frame()))
}

/// The error frame's text: what went wrong.
pub(crate) fn error_frame(problem: &str) -> String {
    format!("the search failed — {problem}")
}

/// The empty frame: a bordered box that names the query that matched
/// nothing, and the way out of it.
pub(crate) fn render_empty_frame(
    query: &str,
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Div {
    frame(theme::text_muted(), theme::border())
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(SharedString::from(empty_frame(query)))
        .child(
            div()
                .id("discover-clear-filters")
                .px_2()
                .py_1()
                .rounded_sm()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .on_click(on_clear)
                .child("clear filters"),
        )
}

/// The error frame: a bordered box in the alarm ink — loud, and the
/// results behind it stay standing.
pub(crate) fn render_error_frame(problem: &str) -> gpui::Div {
    frame(theme::status_error(), theme::status_error())
        .child(SharedString::from(error_frame(problem)))
}

/// A frame: a border, an ink, and the padding the text sits in.
fn frame(ink: Rgba, border: Rgba) -> gpui::Div {
    div()
        .px_3()
        .py_2()
        .rounded_sm()
        .border_1()
        .border_color(border)
        .text_size(RowLayout::small_size())
        .text_color(ink)
}

/// A result row's click: the snapshot rides along, and the callback
/// opens its shadow tab.
pub(crate) type OpenHandler = Rc<dyn Fn(RepoSnapshot, &mut Window, &mut App)>;

/// The result rows: name, stars, language — the catalog-style row over
/// a shadow row, without tags, health or enrichment. A click opens the
/// repository as a shadow tab; the "open first" button stays as the
/// shortcut, but it is no longer the only way in.
pub(crate) fn render_results(results: &[RepoSnapshot], on_open: &OpenHandler) -> gpui::Div {
    let rows: Vec<gpui::AnyElement> = results
        .iter()
        .take(30)
        .map(|snapshot| render_row(snapshot, on_open))
        .collect();
    div().flex().flex_col().gap_1().children(rows)
}

/// One result row: `owner/name`, the stars, the first language —
/// clickable.
fn render_row(snapshot: &RepoSnapshot, on_open: &OpenHandler) -> gpui::AnyElement {
    let handler = Rc::clone(on_open);
    let target = snapshot.clone();
    let language = snapshot
        .languages
        .first()
        .map(|language| language.name.as_str())
        .unwrap_or("—");
    div()
        .id(SharedString::from(format!(
            "discover-result-{}-{}",
            snapshot.owner, snapshot.name
        )))
        .px_3()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::text())
        .on_click(move |_, window, cx| handler(target.clone(), window, cx))
        .child(SharedString::from(format!(
            "{}/{} · {} stars · {}",
            snapshot.owner, snapshot.name, snapshot.stars, language
        )))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debounce_blocks_a_second_immediate_run() {
        let mut state = DiscoverState::default();
        assert!(state.can_run());
        state.last_run = Some(Instant::now());
        assert!(!state.can_run(), "one query per interaction");
        state.running = true;
        assert!(!state.can_run());
    }

    #[test]
    fn the_counter_line_is_quiet_without_a_budget() {
        let state = DiscoverState::default();
        assert_eq!(state.counter_line(), None);
        let budgeted = DiscoverState {
            remaining: Some(29),
            ..DiscoverState::default()
        };
        assert_eq!(
            budgeted.counter_line(),
            Some("search: 29 queries left this minute".to_string())
        );
    }

    #[test]
    fn the_default_query_is_the_ohnice_shape() {
        let query = DiscoverQuery::default();
        assert_eq!(query.sort, yagna_core::model::DiscoverSort::Stars);
        assert!(query.min_stars > 0);
    }

    #[test]
    fn the_controls_are_labelled_and_counted() {
        assert_eq!(DAYS.label, "created within");
        assert_eq!(DAYS.unit.map(|unit| unit.word(30)), Some("days"));
        assert_eq!(DAYS.unit.map(|unit| unit.word(1)), Some("day"));
        assert_eq!(STARS.label, "minimum stars");
        assert_eq!(STARS.unit.map(|unit| unit.word(200)), Some("stars"));
        assert_eq!(LANGUAGE.label, "language");
        assert!(LANGUAGE.unit.is_none(), "a language has no unit");
        assert_eq!(TOPIC.label, "topic (one)");
        assert!(TOPIC.unit.is_none(), "a topic has no unit");
        assert_eq!(WORD.label, "word");
        assert!(WORD.unit.is_none(), "a word has no unit");
    }

    #[test]
    fn the_query_label_names_the_filters() {
        let state = DiscoverState::default();
        assert_eq!(state.query_label(), "created:>30 stars:>200 language:rust");
    }

    #[test]
    fn the_query_label_names_the_topic_and_quotes_the_word() {
        let state = DiscoverState {
            query: DiscoverQuery {
                topic: Some("wasm".to_string()),
                word: Some("web server".to_string()),
                ..DiscoverQuery::default()
            },
            ..DiscoverState::default()
        };
        assert_eq!(
            state.query_label(),
            "created:>30 stars:>200 language:rust topic:wasm \"web server\""
        );
    }

    #[test]
    fn the_empty_frame_names_the_query() {
        let frame = empty_frame("created:>30 stars:>200");
        assert!(frame.contains("no matches for"), "{frame}");
        assert!(frame.contains("created:>30 stars:>200"), "{frame}");
    }

    #[test]
    fn the_error_frame_carries_the_problem() {
        let frame = error_frame("the network said no");
        assert!(frame.contains("the network said no"), "{frame}");
        assert_ne!(frame, empty_frame("created:>30 stars:>200"));
    }

    #[test]
    fn the_hint_states_the_rate_and_the_debounce() {
        let hint = hint_line();
        assert!(hint.contains("30 requests per minute"), "{hint}");
        assert!(hint.contains("3 seconds"), "{hint}");
    }

    #[test]
    fn the_budget_line_says_what_is_left() {
        let state = DiscoverState {
            remaining: Some(29),
            ..DiscoverState::default()
        };
        let line = state.counter_line().expect("the budget is known");
        assert!(line.contains("queries left this minute"), "{line}");
        assert!(line.contains("29"), "{line}");
    }

    #[test]
    fn a_run_that_matched_nothing_says_so() {
        let mut state = DiscoverState::default();
        assert!(!state.matched_nothing(), "no search ran yet");
        state.record_success();
        assert!(state.matched_nothing());
    }

    #[test]
    fn a_failed_run_is_not_an_empty_result() {
        let mut state = DiscoverState::default();
        state.record_failure("the network said no");
        assert!(state.error.is_some());
        assert!(!state.matched_nothing(), "the failure speaks for itself");
    }

    /// The sort picker offers both orders, the chosen one marked, and
    /// the index names its order.
    #[test]
    fn the_sort_picker_offers_both_orders_and_names_them() {
        let mut state = DiscoverState::default();
        assert_eq!(state.sort_label(), "stars");
        let options = sort_options(&state);
        assert_eq!(options.len(), 2);
        assert!(options[0].selected, "stars is the default");
        assert!(!options[1].selected);
        state.set_sort(sort_at(1).expect("the second order"));
        assert_eq!(state.sort_label(), "newest");
        assert!(sort_at(2).is_none(), "there is no third order");
    }

    #[test]
    fn clearing_the_filters_puts_the_query_back() {
        let mut state = DiscoverState {
            query: DiscoverQuery {
                created_within_days: 7,
                min_stars: 900,
                language: Some("rust".to_string()),
                topic: Some("wasm".to_string()),
                word: Some("parser".to_string()),
                sort: DiscoverSort::Newest,
            },
            results: vec![RepoSnapshot {
                forge: yagna_core::model::Forge::GitHub,
                repo_id: 1,
                owner: "owner".to_string(),
                name: "name".to_string(),
                description: None,
                stars: 1,
                languages: Vec::new(),
                license: None,
                topics: Vec::new(),
                default_branch: None,
                created_at: chrono::Utc::now(),
                last_commit_at: chrono::Utc::now(),
                starred_at: None,
                commit_count: 0,
                latest_release: None,
                is_fork: false,
                is_archived: false,
            }],
            error: Some("it went wrong".to_string()),
            searched: true,
            ..DiscoverState::default()
        };
        state.clear_filters();
        assert_eq!(state.query, DiscoverQuery::default());
        assert!(state.results.is_empty());
        assert!(state.error.is_none());
        assert!(!state.searched);
    }
}
