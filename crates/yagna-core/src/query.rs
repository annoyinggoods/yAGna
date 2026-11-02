//! The deep search's query grammar: the AST every store query starts
//! from, and the parser that produces it. The grammar is a fixed table
//! — terms, quoted phrases, exclusions, and five field keys — and
//! anything richer is out of scope by construction.
//!
//! ```text
//! rust cli                     two terms, both must match
//! "exact phrase"               a phrase
//! rust -list                   a term with an exclusion
//! lang:rust license:apache-2.0 text-field filters
//! tag:reading                  a user tag filter
//! stars:>1000 health:>8        number-field filters (>, <)
//! ```
//!
//! The parser, not the store, owns validation: a query that does not
//! parse is a `QueryError` before any SQL exists. A word that looks
//! like `key:value` but whose key is not one of the five stays a plain
//! term — it is searchable text, not a silent filter. A query with no
//! positive part (only exclusions) is rejected: it would match nothing
//! or everything, and neither is an answer.
//!
//! Items: `Query` — the AST; `FieldKey`/`FieldOp` — the field filters'
//! shape; `QueryError` — where and why a query did not parse; `parse` —
//! text to AST; `to_text` — AST back to text; `Clause` — one leaf of a
//! query as a search bar shows it; `clauses_of`/`compose` — the leaf
//! view and its inverse; `FIELDS` — the fixed field table.
//! Depends on: thiserror (the error).
//! Called by: yagna-store (the AST is what `Store::search` executes),
//! the app (the deep-search panel parses before it asks the store).

/// The AST one parsed query is. `And` is the only combinator — the
/// grammar has no OR by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// A bare word: FTS-matched as a string.
    Term(String),
    /// A quoted phrase: FTS-matched whole, spaces included.
    Phrase(String),
    /// A negated node — `-rust`, `-"exact phrase"`, `-lang:rust`.
    Exclude(Box<Query>),
    /// Every child must match. The parser folds the whole query's nodes
    /// into one `And` when there is more than one.
    And(Vec<Query>),
    /// A field filter: `lang:rust`, `stars:>1000`.
    Field {
        key: FieldKey,
        op: FieldOp,
        value: String,
    },
}

/// The field keys the grammar knows. Anything else is not a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
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

impl FieldKey {
    /// The key's canonical spelling — the text the parser accepts. The
    /// field table is the one place a spelling is written down, so the
    /// serializer and the parser can never disagree about it. The table
    /// holds one canonical row per key, so the lookup always answers;
    /// an alias row never shadows the canonical one.
    pub fn spelling(self) -> &'static str {
        FIELDS
            .iter()
            .find(|(_, key, _)| *key == self)
            .map(|(spelling, _, _)| *spelling)
            .unwrap_or_default()
    }
}

/// What a field's value means. Text fields take a bare value; number
/// fields take a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldOp {
    /// The value matches (case-insensitively, in the store).
    Match,
    /// The number must exceed the value.
    Greater,
    /// The number must be under the value.
    Less,
}

impl FieldOp {
    /// The operator's spelling in query text: `Match` writes nothing at
    /// all, the two comparisons write their sign.
    pub fn spelling(self) -> &'static str {
        match self {
            FieldOp::Match => "",
            FieldOp::Greater => ">",
            FieldOp::Less => "<",
        }
    }
}

/// The field table's value kinds: text fields take a bare value, number
/// fields take a comparison and plain digits.
enum ValueKind {
    Text,
    Number,
}

/// The field table: the key's spelling, its AST key, and the kind of
/// value it takes. This table is the grammar's whole field half.
const FIELDS: [(&str, FieldKey, ValueKind); 11] = [
    ("lang", FieldKey::Lang, ValueKind::Text),
    ("language", FieldKey::Lang, ValueKind::Text),
    ("license", FieldKey::License, ValueKind::Text),
    ("tag", FieldKey::Tag, ValueKind::Text),
    ("stars", FieldKey::Stars, ValueKind::Number),
    ("health", FieldKey::Health, ValueKind::Number),
    ("repo", FieldKey::Repo, ValueKind::Text),
    ("owner", FieldKey::Owner, ValueKind::Text),
    ("topic", FieldKey::Topic, ValueKind::Text),
    ("readme", FieldKey::Readme, ValueKind::Text),
    ("digest", FieldKey::Digest, ValueKind::Text),
];

/// Why a query did not parse, and where (the byte offset in the input).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("at byte {at}: {message}")]
pub struct QueryError {
    pub at: usize,
    pub message: String,
}

impl QueryError {
    fn new(at: usize, message: impl Into<String>) -> Self {
        Self {
            at,
            message: message.into(),
        }
    }
}

/// Parses one query line into the AST. Empty input, an unclosed phrase,
/// a number field without a comparison, and a query with no positive
/// part are all errors before any SQL exists.
pub fn parse(input: &str) -> Result<Query, QueryError> {
    let nodes = leaf_nodes(input)?;
    let query = single_or_and(nodes)?;
    if !has_positive(&query) {
        return Err(QueryError::new(
            input.len(),
            "the query excludes everything and matches nothing",
        ));
    }
    Ok(query)
}

/// Serializes an AST back into the query text the parser accepts: a
/// term is a bare word, a phrase is quoted, an exclusion is a dash in
/// front of its node, and a field is its key, a colon and its value.
pub fn to_text(query: &Query) -> String {
    match query {
        Query::Term(word) => word.clone(),
        Query::Phrase(phrase) => format!("\"{phrase}\""),
        Query::Exclude(inner) => format!("-{}", to_text(inner)),
        Query::And(children) => children.iter().map(to_text).collect::<Vec<_>>().join(" "),
        Query::Field { key, op, value } => {
            format!("{}:{}{}", key.spelling(), op.spelling(), value)
        }
    }
}

/// One clause of a query, as a UI shows it: a term, a phrase, a field
/// filter, or one of those excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause {
    Term(String),
    Phrase(String),
    Field {
        key: FieldKey,
        op: FieldOp,
        value: String,
    },
    Excluded(Box<Clause>),
}

/// The query's top-level leaves in order — the chips a search bar shows
/// and edits. An `And` contributes its children; any other node
/// contributes one clause.
pub fn clauses_of(query: &Query) -> Vec<Clause> {
    clauses_in(std::slice::from_ref(query))
}

/// The clauses of a list of nodes: an `And` contributes its children,
/// every other node contributes its own clause.
fn clauses_in(nodes: &[Query]) -> Vec<Clause> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Query::And(children) => out.extend(clauses_in(children)),
            // A leaf contributes its one clause; the option is `None`
            // only for a container, which has no clause of its own.
            _ => out.extend(clause_of(node)),
        }
    }
    out
}

/// One node's clause, when the node is a leaf: a term, a phrase or a
/// field filter. An `And` is a container rather than a leaf, so it has
/// no clause of its own.
fn clause_of(node: &Query) -> Option<Clause> {
    match node {
        Query::Term(word) => Some(Clause::Term(word.clone())),
        Query::Phrase(phrase) => Some(Clause::Phrase(phrase.clone())),
        Query::Field { key, op, value } => Some(Clause::Field {
            key: *key,
            op: *op,
            value: value.clone(),
        }),
        Query::Exclude(inner) => Some(Clause::Excluded(Box::new(clause_of(inner)?))),
        Query::And(_) => None,
    }
}

/// The inverse of [`clauses_of`]: the query the clauses stand for. No
/// clauses are no query; one clause is that clause's node; several
/// clauses are an `And` of them.
pub fn compose(clauses: &[Clause]) -> Option<Query> {
    let mut nodes = clauses.iter().map(node_of);
    let first = nodes.next()?;
    let Some(second) = nodes.next() else {
        return Some(first);
    };
    let mut all = vec![first, second];
    all.extend(nodes);
    Some(Query::And(all))
}

/// One clause's node: the clause's own shape, without the `And` that
/// `compose` wraps several of them in.
fn node_of(clause: &Clause) -> Query {
    match clause {
        Clause::Term(word) => Query::Term(word.clone()),
        Clause::Phrase(phrase) => Query::Phrase(phrase.clone()),
        Clause::Field { key, op, value } => Query::Field {
            key: *key,
            op: *op,
            value: value.clone(),
        },
        Clause::Excluded(inner) => Query::Exclude(Box::new(node_of(inner))),
    }
}

/// Folds the leaf nodes: one node stands alone, several fold into an
/// `And`, none is an empty query.
fn single_or_and(nodes: Vec<Query>) -> Result<Query, QueryError> {
    let mut iter = nodes.into_iter();
    let Some(first) = iter.next() else {
        return Err(QueryError::new(0, "the query is empty"));
    };
    let Some(second) = iter.next() else {
        return Ok(first);
    };
    let mut all = vec![first, second];
    all.extend(iter);
    Ok(Query::And(all))
}

/// Whether the query would match anything at all: a term, a phrase, or
/// a field filter counts; an exclusion does not.
fn has_positive(query: &Query) -> bool {
    match query {
        Query::Term(_) | Query::Phrase(_) | Query::Field { .. } => true,
        Query::Exclude(_) => false,
        Query::And(children) => children.iter().any(has_positive),
    }
}

/// Splits the input into its leaf nodes: whitespace separates them,
/// quotes make phrases, a leading dash excludes the node after it.
fn leaf_nodes(input: &str) -> Result<Vec<Query>, QueryError> {
    let mut nodes = Vec::new();
    let mut rest = input;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Ok(nodes);
        }
        let at = input.len() - rest.len();
        let (node, tail) = next_node(rest, at)?;
        nodes.push(node);
        rest = tail;
    }
}

/// The next leaf node at the front of `rest`, with the text that
/// follows it. `at` is the node's absolute byte offset in the input —
/// the offset the error reports.
fn next_node(rest: &str, at: usize) -> Result<(Query, &str), QueryError> {
    let Some(first) = rest.chars().next() else {
        return Err(QueryError::new(at, "the query ends unexpectedly"));
    };
    match first {
        '"' => quoted_node(rest, at),
        '-' => excluded_node(rest, at),
        _ => word_node(rest, at),
    }
}

/// A quoted phrase node: everything to the closing quote. An unclosed
/// phrase is an error, not a guess.
fn quoted_node(rest: &str, at: usize) -> Result<(Query, &str), QueryError> {
    let inner = &rest[1..];
    match inner.find('"') {
        Some(end) => {
            let phrase = &inner[..end];
            if phrase.trim().is_empty() {
                return Err(QueryError::new(at, "the phrase is empty"));
            }
            Ok((Query::Phrase(phrase.to_string()), &inner[end + 1..]))
        }
        None => Err(QueryError::new(at, "the phrase is not closed")),
    }
}

/// An excluded node: `-` and then one node. An exclusion of an
/// exclusion is refused — the grammar has no double negation.
fn excluded_node(rest: &str, at: usize) -> Result<(Query, &str), QueryError> {
    let inner_at = at + 1 + (rest[1..].len() - rest[1..].trim_start().len());
    let tail = rest[1..].trim_start();
    if tail.starts_with('-') {
        return Err(QueryError::new(
            at,
            "an exclusion cannot exclude an exclusion",
        ));
    }
    if tail.is_empty() {
        return Err(QueryError::new(at, "the exclusion has nothing to exclude"));
    }
    let (inner, tail) = next_node(tail, inner_at)?;
    Ok((Query::Exclude(Box::new(inner)), tail))
}

/// A bare word: up to the next separator, then classified — a field
/// when its key is in the table, a term when it is not.
fn word_node(rest: &str, at: usize) -> Result<(Query, &str), QueryError> {
    let end = rest
        .char_indices()
        .find(|(_, character)| character.is_whitespace() || *character == '"')
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    let word = &rest[..end];
    if word.is_empty() {
        return Err(QueryError::new(at, "the query has an empty word"));
    }
    let node = classify_word(word, at)?;
    Ok((node, &rest[end..]))
}

/// The word's shape: `key:value` with a known key is a field filter;
/// anything else is searchable text.
fn classify_word(word: &str, at: usize) -> Result<Query, QueryError> {
    let Some((key, value)) = word.split_once(':') else {
        return Ok(Query::Term(word.to_string()));
    };
    let Some((_, field_key, kind)) = FIELDS
        .iter()
        .find(|(spelling, _, _)| spelling.eq_ignore_ascii_case(key))
    else {
        return Ok(Query::Term(word.to_string()));
    };
    field_of(*field_key, kind, value, at)
}

/// The field's value, validated by its kind: text fields take a bare
/// value, number fields take `>digits` or `<digits`.
fn field_of(key: FieldKey, kind: &ValueKind, value: &str, at: usize) -> Result<Query, QueryError> {
    match kind {
        ValueKind::Text => text_field(key, value, at),
        ValueKind::Number => number_field(key, value, at),
    }
}

/// A text field's value: present, and no comparison — `lang:>rust` is
/// a mistake the grammar refuses rather than a filter that never fires.
fn text_field(key: FieldKey, value: &str, at: usize) -> Result<Query, QueryError> {
    if value.is_empty() {
        return Err(QueryError::new(at, "the field filter has no value"));
    }
    if value.starts_with('>') || value.starts_with('<') {
        return Err(QueryError::new(
            at,
            "this field takes a value, not a comparison",
        ));
    }
    Ok(Query::Field {
        key,
        op: FieldOp::Match,
        value: value.to_string(),
    })
}

/// A number field's value: a comparison and plain digits.
fn number_field(key: FieldKey, value: &str, at: usize) -> Result<Query, QueryError> {
    let (op, digits) = match value.chars().next() {
        Some('>') => (FieldOp::Greater, &value[1..]),
        Some('<') => (FieldOp::Less, &value[1..]),
        _ => {
            return Err(QueryError::new(
                at,
                "a number field needs > or < before its number",
            ));
        }
    };
    let number: u64 = digits
        .parse()
        .map_err(|_| QueryError::new(at, "the number is not plain digits"))?;
    Ok(Query::Field {
        key,
        op,
        value: number.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(text: &str) -> Query {
        Query::Term(text.to_string())
    }

    fn phrase(text: &str) -> Query {
        Query::Phrase(text.to_string())
    }

    fn excluded(node: Query) -> Query {
        Query::Exclude(Box::new(node))
    }

    fn field(key: FieldKey, op: FieldOp, value: &str) -> Query {
        Query::Field {
            key,
            op,
            value: value.to_string(),
        }
    }

    /// Every grammar feature, table-driven: the input and the AST it
    /// must parse into.
    #[test]
    fn every_grammar_feature_parses_to_its_ast() {
        let cases: Vec<(&str, Query)> = vec![
            ("rust", term("rust")),
            ("  spaced  ", term("spaced")),
            ("rust cli", Query::And(vec![term("rust"), term("cli")])),
            ("\"exact phrase\"", phrase("exact phrase")),
            ("\"quoted with rust\"", phrase("quoted with rust")),
            (
                "rust \"exact phrase\"",
                Query::And(vec![term("rust"), phrase("exact phrase")]),
            ),
            (
                "rust -list",
                Query::And(vec![term("rust"), excluded(term("list"))]),
            ),
            (
                "rust -\"some list\"",
                Query::And(vec![term("rust"), excluded(phrase("some list"))]),
            ),
            (
                "-lang:rust stars:>100",
                Query::And(vec![
                    excluded(field(FieldKey::Lang, FieldOp::Match, "rust")),
                    field(FieldKey::Stars, FieldOp::Greater, "100"),
                ]),
            ),
            ("lang:rust", field(FieldKey::Lang, FieldOp::Match, "rust")),
            (
                "LICENSE:MIT",
                field(FieldKey::License, FieldOp::Match, "MIT"),
            ),
            (
                "tag:reading",
                field(FieldKey::Tag, FieldOp::Match, "reading"),
            ),
            ("stars:<50", field(FieldKey::Stars, FieldOp::Less, "50")),
            ("health:>8", field(FieldKey::Health, FieldOp::Greater, "8")),
            ("foo:bar", term("foo:bar")),
            ("rust:the-language", term("rust:the-language")),
        ];
        for (input, expected) in &cases {
            let parsed = parse(input).unwrap_or_else(|error| panic!("{input:?} failed: {error}"));
            assert_eq!(&parsed, expected, "input {input:?}");
        }
    }

    #[test]
    fn unknown_keys_stay_searchable_text() {
        let parsed = parse("foo:bar").expect("unknown key");
        assert_eq!(parsed, Query::Term("foo:bar".to_string()));
    }

    #[test]
    fn a_single_field_is_a_whole_query() {
        let parsed = parse("lang:rust").expect("field only");
        assert_eq!(
            parsed,
            field(FieldKey::Lang, FieldOp::Match, "rust"),
            "a lone field needs no And"
        );
    }

    #[test]
    fn empty_and_blank_queries_are_errors() {
        let error = parse("").expect_err("empty");
        assert_eq!(error.at, 0);
        assert!(parse("   ").is_err(), "blank is empty too");
    }

    #[test]
    fn exclusions_only_are_rejected() {
        let error = parse("-rust").expect_err("exclusion only");
        assert!(
            error.message.contains("matches nothing"),
            "the error says why: {error}"
        );
        assert!(parse("-lang:rust stars:>100").is_ok(), "a field saves it");
    }

    #[test]
    fn unclosed_and_empty_phrases_are_errors() {
        let error = parse("\"open ended").expect_err("unclosed");
        assert!(error.message.contains("not closed"), "{error}");
        assert!(parse("\"\"").is_err(), "an empty phrase is not a match");
    }

    #[test]
    fn number_fields_demand_a_comparison_and_digits() {
        let error = parse("stars:1000").expect_err("no comparison");
        assert!(error.message.contains("> or <"), "{error}");
        assert!(parse("stars:>1k").is_err(), "suffixes are not grammar");
        assert!(parse("health:").is_err(), "no digits at all");
        assert!(parse("stars:<0").is_ok(), "zero is digits");
    }

    #[test]
    fn text_fields_refuse_comparisons_and_missing_values() {
        let error = parse("lang:>rust").expect_err("comparison on text");
        assert!(error.message.contains("not a comparison"), "{error}");
        assert!(parse("tag:").is_err(), "no value");
    }

    #[test]
    fn double_exclusions_are_refused() {
        let error = parse("rust --list").expect_err("double exclusion");
        assert!(error.message.contains("exclusion"), "{error}");
        let error = parse("rust -").expect_err("bare dash");
        assert!(error.message.contains("nothing to exclude"), "{error}");
    }

    #[test]
    fn errors_name_the_true_byte_offset() {
        let error = parse("rust \"open ended").expect_err("unclosed");
        assert_eq!(error.at, 5, "the phrase starts after 'rust '");
        let error = parse("lang:rust stars:1k").expect_err("bad number");
        assert_eq!(error.at, 10, "the field starts after 'lang:rust '");
        let error = parse("rust --list").expect_err("double exclusion");
        assert_eq!(error.at, 5, "the exclusion starts after 'rust '");
    }

    #[test]
    fn multi_word_phrases_keep_their_spaces() {
        let parsed = parse("\"a b   c\" tail").expect("phrase with spaces");
        assert_eq!(
            parsed,
            Query::And(vec![phrase("a b   c"), term("tail")]),
            "the phrase keeps its inner spacing; the tail follows"
        );
    }

    /// The queries the serializer and the clause view must handle: a
    /// term, a phrase, an exclusion, a mixed `And`, and every field
    /// key with every operator the grammar gives it.
    fn representative_queries() -> Vec<Query> {
        vec![
            term("rust"),
            phrase("exact phrase"),
            Query::And(vec![term("rust"), term("cli")]),
            Query::And(vec![term("rust"), excluded(term("list"))]),
            Query::And(vec![term("rust"), excluded(phrase("some list"))]),
            Query::And(vec![
                excluded(field(FieldKey::Lang, FieldOp::Match, "rust")),
                field(FieldKey::Stars, FieldOp::Greater, "100"),
            ]),
            field(FieldKey::Lang, FieldOp::Match, "rust"),
            field(FieldKey::License, FieldOp::Match, "MIT"),
            field(FieldKey::Tag, FieldOp::Match, "reading"),
            field(FieldKey::Stars, FieldOp::Greater, "1000"),
            field(FieldKey::Stars, FieldOp::Less, "1000"),
            field(FieldKey::Health, FieldOp::Greater, "8"),
            field(FieldKey::Health, FieldOp::Less, "9"),
        ]
    }

    #[test]
    fn to_text_reparses_to_the_same_ast() {
        for query in representative_queries() {
            let text = to_text(&query);
            let reparsed = parse(&text).expect("the serialized text parses");
            assert_eq!(reparsed, query, "the text {text:?} parses back");
        }
    }

    #[test]
    fn to_text_writes_the_expected_text() {
        let cases: Vec<(Query, &str)> = vec![
            (term("rust"), "rust"),
            (phrase("exact phrase"), "\"exact phrase\""),
            (excluded(term("rust")), "-rust"),
            (field(FieldKey::Lang, FieldOp::Match, "rust"), "lang:rust"),
            (
                field(FieldKey::Stars, FieldOp::Greater, "1000"),
                "stars:>1000",
            ),
            (field(FieldKey::Stars, FieldOp::Less, "1000"), "stars:<1000"),
            (Query::And(vec![term("rust"), term("cli")]), "rust cli"),
        ];
        for (query, expected) in &cases {
            assert_eq!(&to_text(query), expected, "the text of {query:?}");
        }
    }

    #[test]
    fn clauses_and_compose_are_inverses() {
        for query in representative_queries() {
            let clauses = clauses_of(&query);
            let composed = compose(&clauses).expect("the clauses compose");
            assert_eq!(composed, query, "the clauses of {query:?} compose back");
        }
    }

    #[test]
    fn the_clause_list_survives_a_text_round_trip() {
        for query in representative_queries() {
            let clauses = clauses_of(&query);
            let text = to_text(&query);
            let reparsed = parse(&text).expect("the serialized text parses");
            assert_eq!(clauses_of(&reparsed), clauses, "the clauses of {text:?}");
        }
    }

    #[test]
    fn an_excluded_clause_is_an_exclusion() {
        let clause = Clause::Excluded(Box::new(Clause::Term("rust".to_string())));
        assert_eq!(compose(&[clause]), Some(excluded(term("rust"))));
    }

    #[test]
    fn no_clauses_are_no_query() {
        assert_eq!(compose(&[]), None);
    }

    /// The field table is the one place a spelling is written down, so
    /// a key's spelling must parse back as that same key.
    #[test]
    fn invariant_every_field_key_spells_itself_as_a_field() {
        let cases: [(FieldKey, &str, Query); 10] = [
            (
                FieldKey::Lang,
                "lang",
                field(FieldKey::Lang, FieldOp::Match, "x"),
            ),
            (
                FieldKey::License,
                "license",
                field(FieldKey::License, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Tag,
                "tag",
                field(FieldKey::Tag, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Stars,
                "stars",
                field(FieldKey::Stars, FieldOp::Greater, "1"),
            ),
            (
                FieldKey::Health,
                "health",
                field(FieldKey::Health, FieldOp::Greater, "1"),
            ),
            (
                FieldKey::Repo,
                "repo",
                field(FieldKey::Repo, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Owner,
                "owner",
                field(FieldKey::Owner, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Topic,
                "topic",
                field(FieldKey::Topic, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Readme,
                "readme",
                field(FieldKey::Readme, FieldOp::Match, "x"),
            ),
            (
                FieldKey::Digest,
                "digest",
                field(FieldKey::Digest, FieldOp::Match, "x"),
            ),
        ];
        for (key, spelling, expected) in cases {
            assert_eq!(key.spelling(), spelling, "the key spells itself");
            let text = to_text(&expected);
            let parsed = parse(&text).expect("the spelled field parses");
            assert_eq!(parsed, expected, "the spelling {spelling} parses back");
        }
    }

    /// The long spelling of the language field is an alias: it parses
    /// to the same key, and the canonical spelling is what the
    /// serializer writes.
    #[test]
    fn the_long_spelling_of_the_language_field_is_an_alias() {
        let parsed = parse("language:rust").expect("the alias parses");
        assert_eq!(parsed, field(FieldKey::Lang, FieldOp::Match, "rust"));
        assert_eq!(FieldKey::Lang.spelling(), "lang");
    }
}
