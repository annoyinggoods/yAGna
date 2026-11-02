//! The deep search's query builder: the visual way to write the
//! grammar's clauses, beside the query field.
//!
//! The builder edits the text the free-text field holds — the text is
//! the authority, and the chips are that text's clauses. A draft is
//! written as query text and read back by the grammar, so a clause is
//! always what the grammar made of the draft and a refusal is always
//! the grammar's own words. The draft's field and operator are chosen
//! from open lists — the one dropdown component, not a cycle button:
//! the choices are the ones the grammar accepts for the field.
//!
//! Items: `BuilderState` — the clauses, the draft and the last
//! refusal; `DraftField` — the field a draft is built from;
//! `BuilderActions` — what the builder's controls report; `render` —
//! the element.
//! Core functions: `operators_for` — the operators a field's kind
//! allows; `clause_text` — a clause in the grammar's spelling;
//! `op_label` — an operator as the picker shows it.
//! Depends on: yagna_core::query (the grammar and its spelling), gpui
//! (the elements), crate::ui::dropdown (the pickers), crate::ui::field
//! (the value's field), crate::ui::rows (the size ramp),
//! crate::ui::theme (the palette).
//! Called by: crate::ui (the deep-search panel).

use std::rc::Rc;

use gpui::{AnyElement, App, FocusHandle, KeyDownEvent, SharedString, Window, div, prelude::*, px};
use yagna_core::query::{self, Clause, FieldKey, FieldOp};

use super::dropdown::{self, DropdownActions, DropdownOption, Site};
use super::field::TextField;
use super::rows::RowLayout;
use super::theme;

/// The width the draft's value field takes.
const VALUE_WIDTH: gpui::Pixels = px(180.0);

/// The field a draft clause is built from: a bare term, or one of the
/// grammar's field keys. The grammar's field table is private to the
/// core, so the app names the keys once — here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DraftField {
    /// A bare term: searchable text with no field and no operator.
    Text,
    /// The repository's language.
    Lang,
    /// The repository's license.
    License,
    /// One of the user's own tags.
    Tag,
    /// The star count — a number.
    Stars,
    /// The health index — a number.
    Health,
    /// The repository's `owner/name`.
    Repo,
    /// The repository's owner.
    Owner,
    /// One of the repository's GitHub topics.
    Topic,
    /// The README's text.
    Readme,
    /// The digest's text.
    Digest,
}

impl DraftField {
    /// The fields the dropdown walks, in order: the bare term first,
    /// then the grammar's keys.
    pub(crate) const ALL: [DraftField; 11] = [
        DraftField::Text,
        DraftField::Repo,
        DraftField::Readme,
        DraftField::Digest,
        DraftField::Topic,
        DraftField::Lang,
        DraftField::Owner,
        DraftField::License,
        DraftField::Tag,
        DraftField::Stars,
        DraftField::Health,
    ];

    /// The field's label in the dropdown: the grammar's own spelling of
    /// the key, or "term" for a bare word.
    pub(crate) fn label(self) -> &'static str {
        match self.key() {
            Some(key) => key.spelling(),
            None => "term",
        }
    }

    /// The grammar's key behind the field; `None` for a bare term.
    pub(crate) fn key(self) -> Option<FieldKey> {
        match self {
            DraftField::Text => None,
            DraftField::Lang => Some(FieldKey::Lang),
            DraftField::License => Some(FieldKey::License),
            DraftField::Tag => Some(FieldKey::Tag),
            DraftField::Stars => Some(FieldKey::Stars),
            DraftField::Health => Some(FieldKey::Health),
            DraftField::Repo => Some(FieldKey::Repo),
            DraftField::Owner => Some(FieldKey::Owner),
            DraftField::Topic => Some(FieldKey::Topic),
            DraftField::Readme => Some(FieldKey::Readme),
            DraftField::Digest => Some(FieldKey::Digest),
        }
    }

    /// The option the field's picker holds for this field, at the index
    /// the picker's list names.
    pub(crate) fn at(index: usize) -> Option<Self> {
        DraftField::ALL.get(index).copied()
    }

    /// The operator option the operator's picker holds, at the index
    /// the grammar's list for this field names.
    pub(crate) fn op_at(self, index: usize) -> Option<FieldOp> {
        operators_for(self).into_iter().nth(index)
    }
}

/// The operators the grammar accepts for a field. The list is asked of
/// the grammar rather than kept beside it: the grammar is the only
/// place that knows which fields take a comparison, and it says so by
/// refusing `stars:1` while accepting `stars:>1`. A number field
/// offers its two comparisons, a text field its match, and a bare term
/// none.
pub(crate) fn operators_for(field: DraftField) -> Vec<FieldOp> {
    let Some(key) = field.key() else {
        return Vec::new();
    };
    // Each candidate operator with a value of the shape it takes: a
    // word for a match, a number for a comparison.
    const CANDIDATES: [(FieldOp, &str); 3] = [
        (FieldOp::Match, "x"),
        (FieldOp::Greater, "1"),
        (FieldOp::Less, "1"),
    ];
    CANDIDATES
        .iter()
        .filter(|(op, sample)| accepts(key, *op, sample))
        .map(|(op, _)| *op)
        .collect()
}

/// Whether the grammar accepts the field with that operator and a
/// sample value.
fn accepts(key: FieldKey, op: FieldOp, sample: &str) -> bool {
    let text = format!("{}:{}{}", key.spelling(), op.spelling(), sample);
    query::parse(&text).is_ok()
}

/// The builder's state: the clauses of the current query, the draft
/// being written, and the last refusal.
#[derive(Debug, Clone)]
pub(crate) struct BuilderState {
    /// The clauses the current query text holds.
    clauses: Vec<Clause>,
    /// The field the draft is choosing.
    field: DraftField,
    /// The operator the draft is choosing; `None` for a bare term.
    op: Option<FieldOp>,
    /// The operators the grammar gives the chosen field.
    operators: Vec<FieldOp>,
    /// The draft's value.
    value: String,
    /// The last refusal, in the grammar's own words.
    error: Option<String>,
    /// Whether the query text the clauses came from parses.
    synced: bool,
}

impl Default for BuilderState {
    fn default() -> Self {
        Self::new()
    }
}

impl BuilderState {
    /// An empty builder: no clauses, and a bare term as the draft.
    pub(crate) fn new() -> Self {
        let field = DraftField::Text;
        let operators = operators_for(field);
        Self {
            clauses: Vec::new(),
            field,
            op: None,
            operators,
            value: String::new(),
            error: None,
            synced: true,
        }
    }

    /// The clauses the current query text holds.
    pub(crate) fn clauses(&self) -> &[Clause] {
        &self.clauses
    }

    /// The query text the clauses stand for — the grammar's spelling.
    /// No clauses are no query, and no query is no text.
    pub(crate) fn query_text(&self) -> String {
        query::compose(&self.clauses)
            .map(|parsed| query::to_text(&parsed))
            .unwrap_or_default()
    }

    /// Re-derives the clauses from the query text: the text is the
    /// authority. A text the grammar refuses leaves the last reading
    /// in place and says why — the chips never claim a reading the
    /// text does not have.
    pub(crate) fn sync_from_text(&mut self, text: &str) {
        if text.trim().is_empty() {
            self.clauses.clear();
            self.synced = true;
            self.error = None;
            return;
        }
        match query::parse(text) {
            Ok(parsed) => {
                self.clauses = query::clauses_of(&parsed);
                self.synced = true;
                self.error = None;
            }
            Err(error) => {
                self.synced = false;
                self.error = Some(error.message);
            }
        }
    }

    /// Adds the draft clause to the query the text holds and returns
    /// the new query text.
    ///
    /// The text is read first, so nothing the reader typed by hand is
    /// lost. The draft is written as query text and read back by the
    /// grammar: a draft the grammar refuses never reaches the query,
    /// and the refusal is the grammar's own words.
    pub(crate) fn add_draft(&mut self, text: &str) -> Result<String, String> {
        self.sync_from_text(text);
        if !self.synced {
            return Err(self.refusal());
        }
        let clause = match self.draft_clause() {
            Ok(clause) => clause,
            Err(why) => {
                self.error = Some(why.clone());
                return Err(why);
            }
        };
        self.clauses.push(clause);
        self.value.clear();
        self.error = None;
        Ok(self.query_text())
    }

    /// Removes the clause at the index and returns the query text the
    /// remaining clauses stand for. The text is read first, so the
    /// removal is made against what the reader actually has.
    pub(crate) fn remove(&mut self, text: &str, index: usize) -> Result<String, String> {
        self.sync_from_text(text);
        if !self.synced {
            return Err(self.refusal());
        }
        if index >= self.clauses.len() {
            return Err("there is no clause to remove".to_string());
        }
        self.clauses.remove(index);
        self.error = None;
        Ok(self.query_text())
    }

    /// The field the draft is choosing.
    pub(crate) fn draft_field(&self) -> DraftField {
        self.field
    }

    /// Chooses the draft's field: the operators follow the field's
    /// kind, so a number field offers its comparisons and a text
    /// field its match.
    pub(crate) fn set_draft_field(&mut self, field: DraftField) {
        self.field = field;
        self.operators = operators_for(field);
        self.op = self.operators.first().copied();
    }

    /// The operators the draft's field offers.
    pub(crate) fn draft_operators(&self) -> &[FieldOp] {
        &self.operators
    }

    /// The operator the draft is choosing; `None` for a bare term.
    pub(crate) fn draft_op(&self) -> Option<FieldOp> {
        self.op
    }

    /// Chooses the draft's operator. The choice is kept even when the
    /// field's kind does not offer it — the grammar refuses such a
    /// draft when it is added, in its own words.
    pub(crate) fn set_draft_op(&mut self, op: FieldOp) {
        self.op = Some(op);
    }

    /// The draft's value.
    pub(crate) fn draft_value(&self) -> &str {
        &self.value
    }

    /// Replaces the draft's value.
    pub(crate) fn set_draft_value(&mut self, value: &str) {
        self.value = value.to_string();
    }

    /// The last refusal, in the grammar's own words.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The draft as a clause: the grammar's own reading of the draft's
    /// text. A draft the grammar refuses has no clause.
    fn draft_clause(&self) -> Result<Clause, String> {
        let text = self.draft_text();
        let parsed = query::parse(&text).map_err(|error| error.message)?;
        let clauses = query::clauses_of(&parsed);
        let Some(clause) = clauses.into_iter().next() else {
            // The grammar never parses a query without a clause: the
            // empty case is a structural fallback, not a rule.
            return Err("the draft is empty".to_string());
        };
        Ok(clause)
    }

    /// The draft as query text — the text the grammar is asked about.
    fn draft_text(&self) -> String {
        let value = self.value.trim();
        let Some(key) = self.field.key() else {
            return term_text(value);
        };
        let op = self.op.unwrap_or(FieldOp::Match);
        format!("{}:{}{}", key.spelling(), op.spelling(), value)
    }

    /// The refusal to report: the grammar's own words. The fallback
    /// stands for the unreachable case of a broken text with no
    /// message.
    fn refusal(&self) -> String {
        self.error
            .clone()
            .unwrap_or_else(|| "the query text does not parse".to_string())
    }
}

/// A bare term as query text: a word stays a word, and words with
/// spaces in them become a phrase — the grammar has no other way to
/// say them.
fn term_text(value: &str) -> String {
    if value.contains(char::is_whitespace) {
        return query::to_text(&query::Query::Phrase(value.to_string()));
    }
    query::to_text(&query::Query::Term(value.to_string()))
}

/// A clause in the grammar's own spelling — the text a chip shows.
pub(crate) fn clause_text(clause: &Clause) -> String {
    query::compose(std::slice::from_ref(clause))
        .map(|parsed| query::to_text(&parsed))
        .unwrap_or_default()
}

/// A chip was pressed: the clause's index rides along.
pub(crate) type RemoveHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// A keystroke in the value field.
pub(crate) type KeyHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;
/// A control was pressed.
pub(crate) type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the builder's controls report.
pub(crate) struct BuilderActions {
    /// The add button was pressed.
    pub on_add: ClickHandler,
    /// A chip was pressed: the clause's index rides along.
    pub on_remove: RemoveHandler,
    /// A keystroke in the value field.
    pub on_value_key: KeyHandler,
    /// The value field was pressed: the focus goes to it.
    pub on_value_click: ClickHandler,
}

/// The builder's element: the field's picker, the operator's picker,
/// the value's field, the add button, and the chips of the query the
/// text holds. The pickers are the one dropdown component; the pane
/// holds which list is open and where the keyboard is on it.
pub(crate) fn render(
    state: &BuilderState,
    value_field: &mut TextField,
    window: &mut Window,
    actions: &BuilderActions,
    pickers: &PickerState<'_>,
) -> AnyElement {
    let mut controls = div().flex().flex_row().items_start().gap_2();
    controls = controls.child(render_field(state, pickers));
    controls = controls.when_some(render_operator(state, pickers), |row, picker| {
        row.child(picker)
    });
    controls = controls.child(render_value(state, value_field, window, actions));
    controls = controls.child(render_add(actions));
    let mut column = div().flex().flex_col().gap_2().child(controls);
    column = column.child(render_chips(state, actions));
    column = column.when_some(render_error(state), |block, line| block.child(line));
    column.into_any_element()
}

/// What the pickers need from the pane: which dropdown is open, where
/// the keyboard is on its list, the dropdown's own actions, and the
/// two triggers' focus handles.
pub(crate) struct PickerState<'a> {
    /// The one dropdown the pane holds open, when one is.
    pub open: Option<Site>,
    /// The option the keyboard is on, in the open list.
    pub highlight: usize,
    /// The dropdown component's actions.
    pub dropdown: &'a DropdownActions,
    /// The field picker's trigger focus.
    pub field_focus: &'a FocusHandle,
    /// The operator picker's trigger focus.
    pub op_focus: &'a FocusHandle,
}

/// The field's picker: the current field on the trigger, and the
/// grammar's whole field list under it.
fn render_field(state: &BuilderState, pickers: &PickerState<'_>) -> AnyElement {
    let options: Vec<DropdownOption> = DraftField::ALL
        .iter()
        .map(|field| DropdownOption::new(field.label(), *field == state.draft_field()))
        .collect();
    dropdown::render_inline(
        Site::BuilderField,
        state.draft_field().label(),
        pickers.open == Some(Site::BuilderField),
        pickers.highlight,
        &options,
        pickers.field_focus,
        pickers.dropdown,
    )
}

/// The operator's picker: the current operator on the trigger, and the
/// operators the grammar gives the chosen field under it. A field with
/// no operator — a bare term — shows none.
fn render_operator(state: &BuilderState, pickers: &PickerState<'_>) -> Option<AnyElement> {
    let current = state.draft_op()?;
    let options: Vec<DropdownOption> = state
        .draft_operators()
        .iter()
        .map(|op| DropdownOption::new(op_label(*op), *op == current))
        .collect();
    Some(dropdown::render_inline(
        Site::BuilderOp,
        op_label(current),
        pickers.open == Some(Site::BuilderOp),
        pickers.highlight,
        &options,
        pickers.op_focus,
        pickers.dropdown,
    ))
}

/// The operator's label: `=` for a match, the sign for a comparison.
/// The query spelling of a match is empty — the label says what the
/// operator means, not how it is written.
pub(crate) fn op_label(op: FieldOp) -> &'static str {
    match op {
        FieldOp::Match => "=",
        comparison => comparison.spelling(),
    }
}

/// The draft's value: the app's own text field, so the caret and the
/// focus are the field's. The state is the authority: the field is
/// brought in line with the draft before it is painted.
fn render_value(
    state: &BuilderState,
    value_field: &mut TextField,
    window: &mut Window,
    actions: &BuilderActions,
) -> gpui::Stateful<gpui::Div> {
    if value_field.value() != state.draft_value() {
        value_field.set(state.draft_value());
    }
    let on_key = Rc::clone(&actions.on_value_key);
    let on_click = Rc::clone(&actions.on_value_click);
    value_field
        .render(
            window,
            move |event, window, cx| on_key(event, window, cx),
            move |window, cx| on_click(window, cx),
        )
        .w(VALUE_WIDTH)
}

/// The add button: the draft becomes a clause.
fn render_add(actions: &BuilderActions) -> AnyElement {
    let on_add = Rc::clone(&actions.on_add);
    div()
        .id("builder-add")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .bg(theme::surface_alt())
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| on_add(window, cx))
        .child("add clause")
        .into_any_element()
}

/// The query's clauses as chips: one per clause.
fn render_chips(state: &BuilderState, actions: &BuilderActions) -> AnyElement {
    let chips: Vec<AnyElement> = state
        .clauses()
        .iter()
        .enumerate()
        .map(|(index, clause)| render_chip(index, clause, actions))
        .collect();
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_1()
        .children(chips)
        .into_any_element()
}

/// One clause's chip: the clause in the grammar's spelling, and the
/// remove action that takes it away.
fn render_chip(index: usize, clause: &Clause, actions: &BuilderActions) -> AnyElement {
    let on_remove = Rc::clone(&actions.on_remove);
    let label = format!("{} ×", clause_text(clause));
    div()
        .id(SharedString::from(format!("builder-chip-{index}")))
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(theme::surface_alt())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| on_remove(index, window, cx))
        .child(SharedString::from(label))
        .into_any_element()
}

/// The last refusal, in the grammar's own words.
fn render_error(state: &BuilderState) -> Option<AnyElement> {
    let error = state.error()?;
    Some(
        div()
            .text_size(RowLayout::small_size())
            .text_color(theme::status_error())
            .child(SharedString::from(error.to_string()))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagna_core::query::Query;

    /// The clauses the representative combinations stand for: a term, a
    /// phrase, an exclusion, a text field, a number field, and a mix.
    fn representative_clauses() -> Vec<Vec<Clause>> {
        vec![
            vec![Clause::Term("rust".to_string())],
            vec![Clause::Phrase("exact phrase".to_string())],
            vec![
                Clause::Term("rust".to_string()),
                Clause::Excluded(Box::new(Clause::Term("list".to_string()))),
            ],
            vec![Clause::Field {
                key: FieldKey::Lang,
                op: FieldOp::Match,
                value: "rust".to_string(),
            }],
            vec![
                Clause::Field {
                    key: FieldKey::Stars,
                    op: FieldOp::Greater,
                    value: "1000".to_string(),
                },
                Clause::Field {
                    key: FieldKey::Health,
                    op: FieldOp::Less,
                    value: "9".to_string(),
                },
            ],
            vec![
                Clause::Field {
                    key: FieldKey::License,
                    op: FieldOp::Match,
                    value: "MIT".to_string(),
                },
                Clause::Field {
                    key: FieldKey::Tag,
                    op: FieldOp::Match,
                    value: "reading".to_string(),
                },
            ],
        ]
    }

    /// A builder holding the given clauses: the clauses are read back
    /// from the text that stands for them, as they are in the app.
    fn state_of(clauses: &[Clause]) -> BuilderState {
        let text = query::compose(clauses)
            .map(|parsed| query::to_text(&parsed))
            .unwrap_or_default();
        let mut state = BuilderState::new();
        state.sync_from_text(&text);
        state
    }

    /// The builder composes a query from its clauses, and the composed
    /// text parses back to the same clauses.
    #[test]
    fn the_composed_text_parses_back_to_the_same_clauses() {
        for clauses in representative_clauses() {
            let state = state_of(&clauses);
            let text = state.query_text();
            let parsed = query::parse(&text).expect("the composed text parses");
            assert_eq!(
                query::clauses_of(&parsed),
                clauses,
                "the text {text:?} parses back to the same clauses"
            );
        }
    }

    /// Adding a clause appends it, and the composed text parses back
    /// to both clauses.
    #[test]
    fn adding_a_clause_appends_it_to_the_query() {
        let mut state = BuilderState::new();
        state.set_draft_field(DraftField::Lang);
        state.set_draft_value("rust");
        let text = state.add_draft("").expect("the draft is accepted");
        assert_eq!(text, "lang:rust", "the clause stands alone in the query");
        state.set_draft_field(DraftField::Stars);
        state.set_draft_op(FieldOp::Greater);
        state.set_draft_value("1000");
        let text = state.add_draft(&text).expect("the draft is accepted");
        let parsed = query::parse(&text).expect("the composed text parses");
        let clauses = query::clauses_of(&parsed);
        assert_eq!(clauses.len(), 2, "both clauses are in the query");
        assert_eq!(
            clauses[0],
            Clause::Field {
                key: FieldKey::Lang,
                op: FieldOp::Match,
                value: "rust".to_string(),
            }
        );
        assert_eq!(
            clauses[1],
            Clause::Field {
                key: FieldKey::Stars,
                op: FieldOp::Greater,
                value: "1000".to_string(),
            }
        );
        assert_eq!(state.clauses(), clauses.as_slice());
    }

    /// Removing a clause drops exactly that one and leaves the others.
    #[test]
    fn removing_a_clause_drops_exactly_that_one() {
        let clauses = representative_clauses()[4].clone();
        let mut state = state_of(&clauses);
        let text = state.query_text();
        let text = state.remove(&text, 0).expect("the clause is removed");
        let parsed = query::parse(&text).expect("the composed text parses");
        let remaining = query::clauses_of(&parsed);
        assert_eq!(remaining.len(), 1, "one clause is left");
        assert_eq!(remaining[0], clauses[1], "the other clause stands");
        assert_eq!(state.clauses(), remaining.as_slice());
    }

    /// A number field with a value the grammar refuses is refused in
    /// the grammar's own words, and the query never changes.
    #[test]
    fn a_refused_clause_is_refused_in_the_grammars_words() {
        let mut state = BuilderState::new();
        state.set_draft_field(DraftField::Stars);
        state.set_draft_op(FieldOp::Greater);
        state.set_draft_value("1k");
        let error = state
            .add_draft("")
            .expect_err("the grammar refuses the digits");
        assert_eq!(error, "the number is not plain digits");
        assert_eq!(state.error(), Some("the number is not plain digits"));
        assert!(state.clauses().is_empty(), "the refused clause never lands");
    }

    /// A comparison on a text field is refused the same way.
    #[test]
    fn a_comparison_on_a_text_field_is_refused() {
        let mut state = BuilderState::new();
        state.set_draft_field(DraftField::Lang);
        state.set_draft_op(FieldOp::Greater);
        state.set_draft_value("rust");
        let error = state
            .add_draft("")
            .expect_err("the grammar refuses the comparison");
        assert_eq!(error, "this field takes a value, not a comparison");
        assert!(state.clauses().is_empty(), "the refused clause never lands");
    }

    /// The operator list follows the field's kind: a number field
    /// compares, a text field matches, a bare term has no operator.
    #[test]
    fn the_operator_list_follows_the_fields_kind() {
        let text = operators_for(DraftField::Lang);
        assert_eq!(text, vec![FieldOp::Match]);
        let license = operators_for(DraftField::License);
        assert_eq!(license, vec![FieldOp::Match]);
        let tag = operators_for(DraftField::Tag);
        assert_eq!(tag, vec![FieldOp::Match]);
        let stars = operators_for(DraftField::Stars);
        assert_eq!(stars, vec![FieldOp::Greater, FieldOp::Less]);
        let health = operators_for(DraftField::Health);
        assert_eq!(health, vec![FieldOp::Greater, FieldOp::Less]);
        assert!(
            operators_for(DraftField::Text).is_empty(),
            "a bare term has no operator"
        );
    }

    /// The dropdown offers the fields the grammar can filter on, and
    /// every one of them composes a query the grammar accepts.
    #[test]
    fn the_dropdown_offers_the_grammars_fields_and_each_composes() {
        for field in DraftField::ALL {
            let label = field.label();
            if field == DraftField::Text {
                continue;
            }
            let operators = operators_for(field);
            assert!(
                !operators.is_empty(),
                "the field {label} offers an operator"
            );
            let op = operators[0];
            let value = match op {
                FieldOp::Match => "x",
                _ => "1",
            };
            let clause = format!("{label}:{}{value}", op.spelling());
            let composed = query::parse(&clause).expect("the clause parses");
            assert_eq!(
                composed,
                Query::Field {
                    key: field.key().expect("the field has a key"),
                    op,
                    value: value.to_string(),
                },
                "the clause {clause} is the field it names"
            );
        }
    }

    /// The fields the dropdown names are the ones the search grammar
    /// writes down.
    #[test]
    fn the_dropdown_names_the_grammar_fields() {
        let labels: Vec<&str> = DraftField::ALL.iter().map(|field| field.label()).collect();
        for name in ["term", "repo", "readme", "digest", "topic", "lang", "owner"] {
            assert!(labels.contains(&name), "the dropdown offers {name}");
        }
    }

    /// Switching the field switches the operators with it.
    #[test]
    fn switching_the_field_switches_the_operators() {
        let mut state = BuilderState::new();
        state.set_draft_field(DraftField::Stars);
        assert_eq!(
            state.draft_operators().to_vec(),
            vec![FieldOp::Greater, FieldOp::Less]
        );
        assert_eq!(state.draft_op(), Some(FieldOp::Greater));
        state.set_draft_field(DraftField::Lang);
        assert_eq!(state.draft_operators().to_vec(), vec![FieldOp::Match]);
        assert_eq!(state.draft_op(), Some(FieldOp::Match));
        state.set_draft_field(DraftField::Text);
        assert!(state.draft_operators().is_empty());
        assert_eq!(state.draft_op(), None);
    }

    /// An empty query has no clauses, and composing nothing yields
    /// nothing.
    #[test]
    fn an_empty_query_has_no_clauses() {
        let mut state = BuilderState::new();
        assert!(state.clauses().is_empty());
        assert_eq!(state.query_text(), "");
        state.sync_from_text("");
        assert!(state.clauses().is_empty());
        assert_eq!(state.query_text(), "");
        assert_eq!(query::compose(state.clauses()), None);
    }

    /// A chip shows the clause in the grammar's own spelling.
    #[test]
    fn a_chip_shows_the_clause_in_its_query_spelling() {
        let term = Clause::Term("rust".to_string());
        assert_eq!(clause_text(&term), "rust");
        let phrase = Clause::Phrase("exact phrase".to_string());
        assert_eq!(clause_text(&phrase), "\"exact phrase\"");
        let field = Clause::Field {
            key: FieldKey::Stars,
            op: FieldOp::Greater,
            value: "1000".to_string(),
        };
        assert_eq!(clause_text(&field), "stars:>1000");
        let excluded = Clause::Excluded(Box::new(term));
        assert_eq!(clause_text(&excluded), "-rust");
    }

    /// The text is the authority: the chips follow what the reader
    /// typed, and a text the grammar refuses is said out loud.
    #[test]
    fn the_text_is_the_authority() {
        let mut state = BuilderState::new();
        state.sync_from_text("rust cli");
        assert_eq!(state.clauses().len(), 2, "both words are clauses");
        state.sync_from_text("rust cli -");
        assert_eq!(state.clauses().len(), 2, "the last reading stands");
        assert!(state.error().is_some(), "the refusal is loud");
        let error = state
            .add_draft("rust cli -")
            .expect_err("the text does not parse");
        assert_eq!(error, "the exclusion has nothing to exclude");
    }

    /// A bare term with spaces in it becomes a phrase: the grammar has
    /// no other way to say it.
    #[test]
    fn a_bare_term_with_spaces_becomes_a_phrase() {
        let mut state = BuilderState::new();
        state.set_draft_field(DraftField::Text);
        state.set_draft_value("exact phrase");
        let text = state.add_draft("").expect("the draft is accepted");
        assert_eq!(text, "\"exact phrase\"");
        let parsed = query::parse(&text).expect("the text parses");
        assert_eq!(
            query::clauses_of(&parsed),
            vec![Clause::Phrase("exact phrase".to_string())]
        );
    }
}
