//! The deep search: the query AST executed over the full-text indexes.
//!
//! The store composes what the AST describes: the text nodes into one
//! FTS `MATCH` string (quoted, `AND`ed, exclusions `NOT`ed), the field
//! filters into SQL clauses against the `repos` and `repo_tags` rows.
//! The README index answers first, the digest index fills the limit
//! behind it; a repository already hit by its README is not hit again
//! by its digest. The excerpt is FTS5's own `snippet()` — the match
//! bracketed, the cut ellipsed. A query with no text at all (field
//! filters only) searches the catalog without the indexes and carries
//! no snippet.
//!
//! Items: `SearchSource` — which text a hit came from; `SearchHit` —
//! the row, its excerpt, and the source; `Store::search` — the verb;
//! `Plan` — the composed match text and clauses (the store's test
//! surface for what a query becomes).
//! Depends on: rusqlite, yagna_core::query (the AST),
//! yagna_core::model (the rows), crate::catalog (the page projection).
//! Called by: the app's deep-search panel.

use rusqlite::types::Value;
use yagna_core::model::RepoRow;
use yagna_core::query::{FieldKey, FieldOp, Query};

use crate::{Store, StoreError};

/// Which stored text a hit's snippet came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// The repository's README.
    Readme,
    /// The repository's digest.
    Digest,
}

impl SearchSource {
    /// The label the results list shows.
    pub fn label(self) -> &'static str {
        match self {
            SearchSource::Readme => "readme",
            SearchSource::Digest => "digest",
        }
    }
}

/// One deep-search hit: the repository's row as the list renders it,
/// the excerpt the match was found in, and where the excerpt came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub row: RepoRow,
    pub snippet: String,
    pub source: SearchSource,
}

/// The snippet's decoration: the match is bracketed, the cut is
/// ellipsed, and the window is sixteen tokens wide.
const SNIPPET_TOKENS: i64 = 16;

impl Store {
    /// Runs one parsed query over the README and digest indexes. The
    /// README hits come first (rank order), the digest hits fill the
    /// limit behind them, and a repository already hit is not hit
    /// twice. The AST was validated by the parser; a query that got
    /// here parses.
    pub fn search(&self, query: &Query, limit: usize) -> Result<Vec<SearchHit>, StoreError> {
        let plan = Plan::of(query)?;
        let mut hits = Vec::new();
        self.collect_hits("fts_readme", SearchSource::Readme, &plan, limit, &mut hits)?;
        if hits.len() < limit {
            self.collect_hits(
                "fts_digest",
                SearchSource::Digest,
                &plan,
                limit - hits.len(),
                &mut hits,
            )?;
        }
        Ok(hits)
    }

    /// One index's share of the search: the FTS query when the query
    /// has text, the bare catalog scan when it has only fields.
    fn collect_hits(
        &self,
        table: &str,
        source: SearchSource,
        plan: &Plan,
        limit: usize,
        hits: &mut Vec<SearchHit>,
    ) -> Result<(), StoreError> {
        match plan.match_text.as_ref() {
            Some(text) => self.collect_fts_hits(table, source, text, plan, limit, hits),
            None => self.collect_field_hits(source, plan, limit, hits),
        }
    }

    /// The full-text query: the index joined to the catalog, the field
    /// clauses on top, rank order, the limit on top of that.
    fn collect_fts_hits(
        &self,
        table: &str,
        source: SearchSource,
        match_text: &str,
        plan: &Plan,
        limit: usize,
        hits: &mut Vec<SearchHit>,
    ) -> Result<(), StoreError> {
        let mut sql = format!(
            "SELECT {table}.rowid, snippet({table}, 0, '[', ']', '…', {SNIPPET_TOKENS})
               FROM {table} JOIN repos r ON r.id = {table}.rowid
              WHERE r.missing_at IS NULL"
        );
        for clause in &plan.clauses {
            sql.push_str(" AND ");
            sql.push_str(clause);
        }
        // The placeholders do not depend on the AST's composition
        // order: the field params are ?1..?N, the match text sits at
        // ?N+1, the limit at ?N+2 — the SQL text may name them in any
        // order, the values follow the indexes.
        let match_param = plan.params.len() + 1;
        let limit_param = plan.params.len() + 2;
        sql.push_str(" AND ");
        sql.push_str(table);
        sql.push_str(&format!(" MATCH ?{match_param}"));
        sql.push_str(&format!(" ORDER BY rank LIMIT ?{limit_param}"));
        let mut values: Vec<Value> = plan.params.clone();
        values.push(Value::Text(match_text.to_string()));
        values.push(Value::Integer(limit as i64));
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(values.iter()))?;
        let mut ids = Vec::new();
        let mut snippets = Vec::new();
        while let Some(row) = rows.next()? {
            ids.push(row.get::<_, i64>(0)?);
            snippets.push(row.get::<_, String>(1)?);
        }
        append_hits(hits, self.rows_by_ids(&ids)?, ids, snippets, source);
        Ok(())
    }

    /// The field-only query: no text to match, so the catalog's rows
    /// are filtered and ordered by stars, snippetless.
    fn collect_field_hits(
        &self,
        source: SearchSource,
        plan: &Plan,
        limit: usize,
        hits: &mut Vec<SearchHit>,
    ) -> Result<(), StoreError> {
        let mut sql = String::from("SELECT r.id FROM repos r WHERE r.missing_at IS NULL");
        for clause in &plan.clauses {
            sql.push_str(" AND ");
            sql.push_str(clause);
        }
        sql.push_str(&format!(
            " ORDER BY r.stars DESC LIMIT ?{}",
            plan.params.len() + 1
        ));
        let mut values = plan.params.clone();
        values.push(Value::Integer(limit as i64));
        let mut statement = self.conn.prepare(&sql)?;
        let ids = statement.query_map(rusqlite::params_from_iter(values.iter()), |row| {
            row.get::<_, i64>(0)
        })?;
        let mut found = Vec::new();
        for id in ids {
            found.push(id?);
        }
        append_hits(hits, self.rows_by_ids(&found)?, found, Vec::new(), source);
        Ok(())
    }
}

/// Folds one index's answers into the hit list: only rows the catalog
/// still serves, only repositories not hit already.
fn append_hits(
    hits: &mut Vec<SearchHit>,
    rows: Vec<RepoRow>,
    ids: Vec<i64>,
    snippets: Vec<String>,
    source: SearchSource,
) {
    for (index, id) in ids.into_iter().enumerate() {
        if hits.iter().any(|hit| hit.row.repo_rowid == id) {
            continue;
        }
        let Some(row) = rows.iter().find(|row| row.repo_rowid == id) else {
            continue;
        };
        let snippet = snippets.get(index).cloned().unwrap_or_default();
        hits.push(SearchHit {
            row: row.clone(),
            snippet,
            source,
        });
    }
}

/// What one query becomes: the quoted `MATCH` text (none for a
/// field-only query), the SQL clauses, and their parameters in order —
/// the field params are `?1..?N`, the match text (when the query has
/// text) is `?N+1`, the limit is the last placeholder. The indexes do
/// not depend on the order the AST's children composed in.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    match_text: Option<String>,
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl Plan {
    /// Composes the plan. The parser already rejected the empty and the
    /// exclusion-only shapes; a query that arrives here has a positive
    /// part.
    pub(crate) fn of(query: &Query) -> Result<Self, StoreError> {
        let mut plan = Self {
            match_text: None,
            clauses: Vec::new(),
            params: Vec::new(),
        };
        plan.push_query(query)?;
        Ok(plan)
    }

    /// The match text, for the store's tests.
    #[cfg(test)]
    pub(crate) fn text(&self) -> Option<&str> {
        self.match_text.as_deref()
    }

    /// The clauses, for the store's tests.
    #[cfg(test)]
    pub(crate) fn clauses(&self) -> &[String] {
        &self.clauses
    }

    /// Walks the AST. `And` is transparent — the grammar's only
    /// combinator; the leaves compose.
    fn push_query(&mut self, query: &Query) -> Result<(), StoreError> {
        match query {
            Query::Term(word) => {
                self.push_match(quote_fts(word));
                Ok(())
            }
            Query::Phrase(phrase) => {
                self.push_match(quote_fts(phrase));
                Ok(())
            }
            Query::Exclude(inner) => self.push_excluded(inner),
            Query::And(children) => {
                for child in children {
                    self.push_query(child)?;
                }
                Ok(())
            }
            Query::Field { key, op, value } => {
                let clause = self.field_clause(*key, *op, value)?;
                self.clauses.push(clause);
                Ok(())
            }
        }
    }

    /// A positive text node: into the `MATCH` string, `AND`ed with the
    /// rest.
    fn push_match(&mut self, quoted: String) {
        self.match_text = Some(match self.match_text.take() {
            Some(text) => format!("{text} AND {quoted}"),
            None => quoted,
        });
    }

    /// An exclusion: a text node joins the `NOT` chain, a field filter
    /// becomes a negated SQL clause.
    fn push_excluded(&mut self, inner: &Query) -> Result<(), StoreError> {
        match inner {
            Query::Term(word) => {
                let quoted = quote_fts(word);
                self.match_text = Some(match self.match_text.take() {
                    Some(text) => format!("{text} NOT {quoted}"),
                    None => return Err(exclusion_without_terms()),
                });
                Ok(())
            }
            Query::Phrase(phrase) => {
                let quoted = quote_fts(phrase);
                self.match_text = Some(match self.match_text.take() {
                    Some(text) => format!("{text} NOT {quoted}"),
                    None => return Err(exclusion_without_terms()),
                });
                Ok(())
            }
            Query::Field { key, op, value } => {
                let clause = self.field_clause(*key, *op, value)?;
                self.clauses.push(format!("NOT ({clause})"));
                Ok(())
            }
            Query::Exclude(_) | Query::And(_) => Err(uncomposable()),
        }
    }

    /// One field filter as a SQL clause against the catalog, with its
    /// parameter appended. The placeholder is computed from the running
    /// parameter count, because the match text may or may not sit at
    /// `?1`.
    fn field_clause(
        &mut self,
        key: FieldKey,
        op: FieldOp,
        value: &str,
    ) -> Result<String, StoreError> {
        let placeholder = |plan: &mut Self| {
            plan.params.push(Value::Text(value.to_string()));
            format!("?{}", plan.params.len())
        };
        let number = |plan: &mut Self| -> Result<String, StoreError> {
            let number: u64 = value.parse().map_err(|_| {
                StoreError::Parse("a number field's value is not digits".to_string())
            })?;
            plan.params
                .push(Value::Integer(i64::try_from(number).unwrap_or(i64::MAX)));
            Ok(format!("?{}", plan.params.len()))
        };
        // A full-text field: the value is an FTS phrase, so it is
        // quoted before it reaches the index.
        let phrase = |plan: &mut Self| -> Result<String, StoreError> {
            if value.trim().is_empty() {
                return Err(StoreError::Parse("a text field needs a value".to_string()));
            }
            plan.params.push(Value::Text(quote_fts(value.trim())));
            Ok(format!("?{}", plan.params.len()))
        };
        let clause = match (key, op) {
            (FieldKey::Lang, FieldOp::Match) => {
                let param = placeholder(self);
                format!("lower(r.primary_language) = lower({param})")
            }
            (FieldKey::License, FieldOp::Match) => {
                let param = placeholder(self);
                format!("lower(r.license_spdx) = lower({param})")
            }
            (FieldKey::Tag, FieldOp::Match) => {
                let param = placeholder(self);
                format!(
                    "EXISTS (SELECT 1 FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id \
                       WHERE rt.repo_rowid = r.id AND lower(t.name) = lower({param}))"
                )
            }
            (FieldKey::Repo, FieldOp::Match) => {
                let param = placeholder(self);
                format!("lower(r.owner || '/' || r.name) LIKE lower('%' || {param} || '%')")
            }
            (FieldKey::Owner, FieldOp::Match) => {
                let param = placeholder(self);
                format!("lower(r.owner) LIKE lower('%' || {param} || '%')")
            }
            (FieldKey::Topic, FieldOp::Match) => {
                // The value's LIKE metacharacters are matched
                // literally, so the filter stays an element match: a
                // word is a word, never a wildcard by accident.
                let literal = escape_like(value);
                self.params.push(Value::Text(literal));
                let param = format!("?{}", self.params.len());
                format!("lower(r.topics_json) LIKE '%\"' || lower({param}) || '\"%' ESCAPE '\\'")
            }
            (FieldKey::Readme, FieldOp::Match) => {
                let param = phrase(self)?;
                format!(
                    "EXISTS (SELECT 1 FROM fts_readme f WHERE f.rowid = r.id AND f.original MATCH {param})"
                )
            }
            (FieldKey::Digest, FieldOp::Match) => {
                let param = phrase(self)?;
                format!(
                    "EXISTS (SELECT 1 FROM fts_digest f WHERE f.rowid = r.id AND f.body MATCH {param})"
                )
            }
            (FieldKey::Stars, FieldOp::Greater) => format!("r.stars > {}", number(self)?),
            (FieldKey::Stars, FieldOp::Less) => format!("r.stars < {}", number(self)?),
            (FieldKey::Health, FieldOp::Greater) => format!("r.health_score > {}", number(self)?),
            (FieldKey::Health, FieldOp::Less) => format!("r.health_score < {}", number(self)?),
            (
                FieldKey::Lang
                | FieldKey::License
                | FieldKey::Tag
                | FieldKey::Repo
                | FieldKey::Owner
                | FieldKey::Topic
                | FieldKey::Readme
                | FieldKey::Digest,
                _,
            ) => {
                return Err(StoreError::Parse(
                    "a text field takes a value, not a comparison".to_string(),
                ));
            }
            (FieldKey::Stars | FieldKey::Health, FieldOp::Match) => {
                return Err(StoreError::Parse(
                    "a number field needs > or < before its number".to_string(),
                ));
            }
        };
        Ok(clause)
    }
}

/// The parser refuses exclusion-only queries; a plan that gets one
/// anyway is a broken handshake, not a search.
fn exclusion_without_terms() -> StoreError {
    StoreError::Parse("the query has no positive terms to exclude from".to_string())
}

/// The parser never nests exclusions or `And`s under an exclusion.
fn uncomposable() -> StoreError {
    StoreError::Parse("the query shape is not composable".to_string())
}

/// One text node quoted for FTS: a string the tokenizer reads whole,
/// with embedded quotes doubled.
fn quote_fts(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// One value's LIKE metacharacters escaped for a literal match: the
/// backslash first, then the two wildcards, so nothing the reader
/// types widens the pattern.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagna_core::query::parse;

    fn plan_of(input: &str) -> Plan {
        Plan::of(&parse(input).expect("parses")).expect("composes")
    }

    #[test]
    fn terms_compose_into_an_and_chain_of_quoted_strings() {
        let plan = plan_of("rust cli");
        assert_eq!(plan.text(), Some("\"rust\" AND \"cli\""));
        assert!(plan.clauses().is_empty());
    }

    #[test]
    fn phrases_are_quoted_whole_and_exclusions_chain_the_not() {
        let plan = plan_of("\"two words\" rust -list");
        assert_eq!(plan.text(), Some("\"two words\" AND \"rust\" NOT \"list\""));
        assert!(plan.clauses().is_empty());
    }

    #[test]
    fn embedded_quotes_are_doubled_not_interpreted() {
        let term = Query::Term("say \"hi\"".to_string());
        let plan = Plan::of(&term).expect("composes");
        assert_eq!(plan.text(), Some("\"say \"\"hi\"\"\""));
    }

    #[test]
    fn field_filters_become_clauses_with_parameters() {
        let plan = plan_of("lang:rust stars:>1000");
        assert_eq!(plan.text(), None, "a field-only query has no match text");
        assert_eq!(plan.clauses().len(), 2);
        assert_eq!(plan.clauses()[0], "lower(r.primary_language) = lower(?1)");
        assert_eq!(plan.clauses()[1], "r.stars > ?2");
    }

    #[test]
    fn field_placeholders_do_not_depend_on_composition_order() {
        // The same field, before and after the text term: the clause's
        // placeholder is the field's own index either way.
        let first = plan_of("lang:rust rust");
        assert_eq!(first.clauses()[0], "lower(r.primary_language) = lower(?1)");
        assert_eq!(first.text(), Some("\"rust\""));
        let second = plan_of("rust lang:rust");
        assert_eq!(second.clauses()[0], "lower(r.primary_language) = lower(?1)");
        assert_eq!(second.text(), Some("\"rust\""));
        let many = plan_of("rust lang:rust stars:<50 health:>8");
        assert_eq!(many.clauses()[0], "lower(r.primary_language) = lower(?1)");
        assert_eq!(many.clauses()[1], "r.stars < ?2");
        assert_eq!(many.clauses()[2], "r.health_score > ?3");
    }

    #[test]
    fn a_tag_filter_is_an_exists_over_the_attachment() {
        let plan = plan_of("tag:reading");
        assert!(
            plan.clauses()[0].contains("repo_tags rt JOIN tags t"),
            "the clause joins the attachments: {}",
            plan.clauses()[0]
        );
    }

    #[test]
    fn the_repository_and_owner_filters_look_at_the_names() {
        let repo = plan_of("repo:cargo");
        assert_eq!(
            repo.clauses()[0],
            "lower(r.owner || '/' || r.name) LIKE lower('%' || ?1 || '%')"
        );
        let owner = plan_of("owner:rust-lang");
        assert_eq!(
            owner.clauses()[0],
            "lower(r.owner) LIKE lower('%' || ?1 || '%')"
        );
    }

    #[test]
    fn the_topic_filter_matches_a_whole_topic() {
        let plan = plan_of("topic:cli");
        assert_eq!(
            plan.clauses()[0],
            "lower(r.topics_json) LIKE '%\"' || lower(?1) || '\"%' ESCAPE '\\'"
        );
    }

    #[test]
    fn a_topic_value_with_like_metacharacters_matches_literally() {
        let plan = plan_of("topic:rust_2024");
        assert_eq!(
            plan.params[0],
            Value::Text("rust\\_2024".to_string()),
            "the underscore is escaped, not a wildcard"
        );
        let percent = plan_of("topic:100%");
        assert_eq!(
            percent.params[0],
            Value::Text("100\\%".to_string()),
            "the percent is escaped, not a wildcard"
        );
    }

    #[test]
    fn the_readme_and_digest_filters_ask_their_own_index() {
        let readme = plan_of("readme:parser");
        assert!(
            readme.clauses()[0].contains("fts_readme f WHERE f.rowid = r.id"),
            "the clause asks the readme index: {}",
            readme.clauses()[0]
        );
        let digest = plan_of("digest:parser");
        assert!(
            digest.clauses()[0].contains("fts_digest f WHERE f.rowid = r.id"),
            "the clause asks the digest index: {}",
            digest.clauses()[0]
        );
    }

    #[test]
    fn a_full_text_field_with_no_value_is_refused() {
        assert!(parse("readme:").is_err(), "an empty phrase is not a phrase");
        assert!(parse("digest:").is_err(), "an empty phrase is not a phrase");
    }

    #[test]
    fn an_excluded_field_is_a_negated_clause() {
        let plan = plan_of("rust -lang:javascript");
        assert_eq!(plan.text(), Some("\"rust\""));
        assert_eq!(
            plan.clauses()[0],
            "NOT (lower(r.primary_language) = lower(?1))"
        );
    }

    #[test]
    fn a_number_is_clamped_not_wrapped() {
        let plan = plan_of("stars:>18446744073709551615");
        assert_eq!(plan.params[0], Value::Integer(i64::MAX));
    }

    // --- The fixture database: the parsed query run for real.

    use crate::testing::{Scratch, sample_snapshot};

    fn db(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    /// Seeds two repositories, one Rust one JavaScript, and gives the
    /// Rust one a README to search through.
    fn seed_pair(store: &mut Store) -> (u64, u64) {
        store
            .upsert_snapshots(
                &[
                    sample_snapshot("someone", "rusty"),
                    sample_snapshot("someone", "jzsy"),
                ],
                now(),
            )
            .expect("seeded");
        let ids: Vec<i64> = store
            .conn
            .query_row(
                "SELECT group_concat(id) FROM (SELECT id FROM repos ORDER BY id)",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("rowids")
            .split(',')
            .map(|id| id.parse().expect("numeric"))
            .collect();
        let (rusty, jzsy) = (ids[0] as u64, ids[1] as u64);
        store
            .conn
            .execute(
                "UPDATE repos SET primary_language = 'Rust' WHERE id = ?1",
                [ids[0]],
            )
            .expect("rusty");
        store
            .conn
            .execute(
                "UPDATE repos SET primary_language = 'JavaScript', stars = 5000 WHERE id = ?1",
                [ids[1]],
            )
            .expect("jzsy");
        store
            .store_readme(
                rusty,
                "# Rusty\n\na cli written in Rust for reading lists\n",
                now(),
            )
            .expect("readme");
        store
            .store_readme(jzsy, "# Jzsy\n\na javascript reading list manager\n", now())
            .expect("readme");
        (rusty, jzsy)
    }

    #[test]
    fn a_phrase_from_a_readme_returns_that_repo_with_a_snippet() {
        let (_scratch, mut store) = db("search-phrase");
        let (rusty, jzsy) = seed_pair(&mut store);
        // A word both READMEs carry.
        let query = parse("reading").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 2, "both READMEs carry the word");
        assert!(
            hits.iter().any(|hit| hit.row.repo_rowid == rusty as i64
                && hit.source == SearchSource::Readme),
            "the rust repo is hit"
        );
        let first = &hits[0];
        assert!(
            first.snippet.contains('[') && first.snippet.contains(']'),
            "the match is bracketed: {}",
            first.snippet
        );
        // The exact phrase: only the README whose words sit in order.
        // (unicode61 has no stemming — "lists" is not "list".)
        let exact = parse("\"reading lists\"").expect("parses");
        let hits = store.search(&exact, 10).expect("searched");
        assert_eq!(
            hits.iter()
                .map(|hit| hit.row.repo_rowid)
                .collect::<Vec<_>>(),
            vec![rusty as i64],
            "only the exact phrase's repo"
        );
        let _ = jzsy;
    }

    #[test]
    fn field_filters_combine_with_text_terms() {
        let (_scratch, mut store) = db("search-fields");
        let (rusty, jzsy) = seed_pair(&mut store);
        // Both carry the words; only one is Rust.
        let query = parse("reading lang:rust").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].row.repo_rowid, rusty as i64);

        let query = parse("reading stars:>1000").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(
            hits.iter()
                .map(|hit| hit.row.repo_rowid)
                .collect::<Vec<_>>(),
            vec![jzsy as i64],
            "only the starred repo passes the number filter"
        );
    }

    #[test]
    fn an_excluded_term_drops_its_repositories() {
        let (_scratch, mut store) = db("search-exclude");
        let (rusty, jzsy) = seed_pair(&mut store);
        let query = parse("reading -javascript").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        let ids: Vec<i64> = hits.iter().map(|hit| hit.row.repo_rowid).collect();
        assert!(
            ids.contains(&(rusty as i64)),
            "the unexcluded repo survives"
        );
        assert!(!ids.contains(&(jzsy as i64)), "the excluded repo is gone");
    }

    #[test]
    fn a_tag_filter_finds_its_tagged_repository() {
        let (_scratch, mut store) = db("search-tag");
        let (rusty, _) = seed_pair(&mut store);
        store.assign_tag(rusty, "reading", None).expect("tagged");
        let query = parse("tag:READING reading").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1, "the tag matched case-insensitively");
        assert_eq!(hits[0].row.repo_rowid, rusty as i64);
    }

    #[test]
    fn a_digest_hit_carries_the_digest_source_and_opens_second() {
        let (_scratch, mut store) = db("search-digest");
        let (rusty, _) = seed_pair(&mut store);
        // The README does not mention "tarball"; the digest does.
        let spec = yagna_core::digest::standard_spec(&yagna_core::model::RepoRef {
            forge: yagna_core::model::Forge::GitHub,
            owner: "someone".to_string(),
            name: "rusty".to_string(),
        });
        let input = yagna_core::digest::DigestInput {
            repo: yagna_core::model::RepoRef {
                forge: yagna_core::model::Forge::GitHub,
                owner: "someone".to_string(),
                name: "rusty".to_string(),
            },
            files: vec![yagna_core::digest::DigestFile {
                path: "src/tar.rs".to_string(),
                bytes: 30,
                text: Some("the tarball is streamed\n".to_string()),
            }],
            truncated_by: None,
            taken_commit: None,
        };
        store
            .store_digest(rusty, &spec, &input, now())
            .expect("digest");

        let query = parse("tarball").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, SearchSource::Digest);
        assert!(hits[0].snippet.contains("tarball"));

        // A word both texts carry: the README hit comes first and the
        // repository is not hit twice.
        store
            .store_digest(rusty, &spec, &input, now())
            .expect("digest again");
        let query = parse("reading").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert!(
            hits.first()
                .is_some_and(|hit| hit.source == SearchSource::Readme),
            "the readme hit leads"
        );
        assert_eq!(
            hits.iter()
                .filter(|hit| hit.row.repo_rowid == rusty as i64)
                .count(),
            1,
            "one repository, one hit"
        );
    }

    #[test]
    fn a_field_only_query_searches_without_the_indexes() {
        let (_scratch, mut store) = db("search-fields-only");
        seed_pair(&mut store);
        let query = parse("lang:javascript").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snippet, "", "no text matched, so no excerpt");
    }

    /// Two repositories, one carrying the topic `rust` and one the
    /// topic `rusty`: the filter is the topic array's elements, the
    /// way the live search's `topic:` qualifier is — no more a
    /// substring match that finds "rust" inside "rusty".
    #[test]
    fn a_topic_filter_matches_elements_not_substrings() {
        let (_scratch, mut store) = db("search-topic-elements");
        let mut rusty = sample_snapshot("someone", "rusty");
        rusty.topics = vec!["rusty".to_string()];
        let mut ruster = sample_snapshot("someone", "ruster");
        ruster.topics = vec!["rust".to_string()];
        store
            .upsert_snapshots(&[rusty, ruster], now())
            .expect("seeded");

        let query = parse("topic:rust").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1, "\"rust\" is not inside \"rusty\"");
        assert_eq!(hits[0].row.repo_ref.name, "ruster");

        let query = parse("topic:rusty").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].row.repo_ref.name, "rusty");
    }

    /// The underscore is a literal in a topic value: `topic:rust_`
    /// asks for a topic spelled `rust_`, and matches nothing here —
    /// the wildcard reading would have found `rusty`.
    #[test]
    fn a_topic_value_with_a_wildcard_character_matches_literally() {
        let (_scratch, mut store) = db("search-topic-wildcard");
        let mut rusty = sample_snapshot("someone", "rusty");
        rusty.topics = vec!["rusty".to_string()];
        store.upsert_snapshots(&[rusty], now()).expect("seeded");
        let query = parse("topic:rust_").expect("parses");
        let hits = store.search(&query, 10).expect("searched");
        assert!(
            hits.is_empty(),
            "the underscore is literal, so no topic matches: {:?}",
            hits.iter()
                .map(|hit| hit.row.repo_ref.name.clone())
                .collect::<Vec<_>>()
        );
    }
}
