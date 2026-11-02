//! The fifteen tools over the store and forge facades: the agent's
//! hands on the catalog, the local worktrees, and the score spec.
//!
//! Every tool is one function taking its arguments and a `ToolContext`
//! (the store, the token, the data directory, the session's spend
//! ledger and trace). Reads answer from the store; the four live tools
//! reach the forge under the spend policy the server charges; the
//! writes are idempotent by design. Errors are plain messages the
//! agent can act on — they name the fix, not just the failure.
//!
//! Items: `ToolContext` — everything a tool runs against.
//! Core functions: `execute` — one tool call to its result;
//! `spend_kind` — the API budget a tool name spends from.
//! Depends on: yagna_store (the disk door), yagna_forge (the network
//! door), yagna_core::fit + query + fuzzy (the maths), crate::fitspec,
//! crate::sync (the sweep).
//! Called by: crate::mcp::server (the `tools/call` handler).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use yagna_core::fit::ParamKind;
use yagna_core::fuzzy::{FuzzyIndex, SearchItem, haystack};
use yagna_core::model::{
    DiscoverQuery, DiscoverSort, NoteSubject, RepoLiveFacts, RepoRef, RepoRow,
};
use yagna_core::query::Query;
use yagna_store::{Store, TagKind};

#[cfg(test)]
use super::budget::load_policy;
use super::budget::{Admission, Budget, SpendKind};
#[cfg(test)]
use super::trace::TRACE_FILE;
use super::trace::{ActionOutcome, Trace, TraceLine};
use crate::fitspec;

// The forge seam's trait: the live tools call its methods on the
// GitHub implementation the facade builds.
use yagna_forge::Forge as _;

/// Everything a tool runs against: the catalog, the token, the data
/// directory (fit.json and agent.toml live there), and the session's
/// spend ledger and activity trace — the last two shared with the
/// sweep worker.
pub struct ToolContext<'a> {
    pub store: &'a mut Store,
    pub token: Option<String>,
    pub data_dir: PathBuf,
    pub budget: Arc<Mutex<Budget>>,
    pub trace: Arc<Mutex<Trace>>,
    pub sweeping: Arc<AtomicBool>,
}

/// The API budget a tool name spends from. `None` for the store-only
/// reads and writes — the cost column of the tool table.
pub fn spend_kind(name: &str) -> Option<SpendKind> {
    match name {
        "search_github" => Some(SpendKind::Search),
        "get_github_repository" | "refresh_repository" | "sync_catalog" => Some(SpendKind::Core),
        _ => None,
    }
}

/// Executes one tool by name: the arguments through the store and
/// forge facades to a structured result, or a message that names the
/// fix.
pub fn execute(name: &str, arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    match name {
        "search_library" => search_library(arguments, ctx),
        "get_repository" => get_repository(arguments, ctx),
        "read_readme" => read_readme(arguments, ctx),
        "list_vocabulary" => list_vocabulary(ctx),
        "list_local_repositories" => list_local_repositories(ctx),
        "read_local_file" => read_local_file(arguments, ctx),
        "get_fit_spec" => get_fit_spec(ctx),
        "set_tag" => set_tag(arguments, ctx),
        "set_note" => set_note(arguments, ctx),
        "write_fit_spec" => write_fit_spec(arguments, ctx),
        "rescore" => rescore(ctx),
        "search_github" => search_github(arguments, ctx),
        "get_github_repository" => get_github_repository(arguments, ctx),
        "refresh_repository" => refresh_repository(arguments, ctx),
        "sync_catalog" => sync_catalog(ctx),
        _ => Err(format!("tool `{name}` is not in the catalogue")),
    }
}

// --- Argument helpers.

fn str_arg<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("`{key}` is required and must be non-empty text"))
}

/// An optional text argument: present and non-empty wins, absent or
/// blank rides as none.
fn opt_str_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The owner/name pair every repository tool takes. The forge rides
/// optional and defaults to the one the catalog speaks; when it is
/// sent, it is validated so a typo never becomes a silent miss.
fn repo_args(arguments: &Value) -> Result<(String, String), String> {
    let forge = opt_str_arg(arguments, "forge").unwrap_or("github");
    if forge != "github" {
        return Err(format!(
            "forge `{forge}` is not known — the catalog speaks `github`"
        ));
    }
    Ok((
        str_arg(arguments, "owner")?.to_string(),
        str_arg(arguments, "name")?.to_string(),
    ))
}

fn resolve_rowid(store: &Store, owner: &str, name: &str) -> Result<i64, String> {
    store
        .repo_rowid("github", owner, name)
        .map_err(|error| format!("catalog: {error}"))?
        .ok_or_else(|| format!("the catalog has no {owner}/{name} — sync it in first"))
}

fn require_token(ctx: &ToolContext) -> Result<String, String> {
    ctx.token
        .clone()
        .ok_or_else(|| "no GitHub token — set GITHUB_TOKEN or run `gh auth login`".to_string())
}

// --- The catalog reads.

fn search_library(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let query_text = str_arg(arguments, "query")?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    if query_names_a_field(query_text) {
        return grammar_search(query_text, limit, ctx);
    }
    fuzzy_search(query_text, limit, ctx)
}

/// Whether the query names one of the deep-search grammar's fields.
/// A parse failure is not a field query — the fuzzy name search takes
/// any text.
fn query_names_a_field(text: &str) -> bool {
    yagna_core::query::parse(text).is_ok_and(|query| has_field(&query))
}

fn has_field(node: &Query) -> bool {
    match node {
        Query::Field { .. } => true,
        Query::And(children) => children.iter().any(has_field),
        Query::Exclude(inner) => has_field(inner),
        _ => false,
    }
}

fn grammar_search(query_text: &str, limit: usize, ctx: &mut ToolContext) -> Result<Value, String> {
    let parsed = yagna_core::query::parse(query_text)
        .map_err(|error| format!("the query does not parse: {error}"))?;
    let hits = ctx
        .store
        .search(&parsed, limit)
        .map_err(|error| format!("catalog: {error}"))?;
    let mut results = Vec::new();
    for hit in &hits {
        results.push(result_json(
            ctx.store,
            &hit.row,
            Some(hit.snippet.clone()),
            Some(hit.source.label()),
        )?);
    }
    Ok(json!({ "mode": "grammar", "results": results }))
}

fn fuzzy_search(query_text: &str, limit: usize, ctx: &mut ToolContext) -> Result<Value, String> {
    let rows = ctx
        .store
        .search_index_rows()
        .map_err(|error| format!("catalog: {error}"))?;
    let items: Vec<SearchItem> = rows
        .iter()
        .map(|row| {
            let text = haystack(
                &row.owner,
                &row.name,
                row.description.as_deref(),
                &row.topics,
                &row.tags,
            );
            SearchItem::new(row.repo_rowid, &text)
        })
        .collect();
    let index = FuzzyIndex::build(items);
    let ids: Vec<i64> = index
        .search(query_text)
        .iter()
        .take(limit)
        .map(|hit| hit.repo_rowid)
        .collect();
    let rows = ctx
        .store
        .rows_by_ids(&ids)
        .map_err(|error| format!("catalog: {error}"))?;
    let mut results = Vec::new();
    for row in &rows {
        results.push(result_json(ctx.store, row, None, None)?);
    }
    Ok(json!({ "mode": "fuzzy", "results": results }))
}

/// One search hit as the tool surface spells it: the catalog row plus
/// the reader's own note — the notes and tags the result carries.
fn result_json(
    store: &Store,
    row: &RepoRow,
    snippet: Option<String>,
    matched: Option<&str>,
) -> Result<Value, String> {
    let mut entry = row_json(row);
    if let Some(snippet) = snippet {
        entry["snippet"] = json!(snippet);
    }
    if let Some(matched) = matched {
        entry["matched"] = json!(matched);
    }
    let note = store
        .note_of(NoteSubject::Repo(row.repo_rowid))
        .map_err(|error| format!("catalog: {error}"))?;
    entry["note"] = match note {
        Some(text) => json!(text),
        None => Value::Null,
    };
    Ok(entry)
}

/// One catalog row as the tool surface spells it: the identity the
/// agent names, flat, with the two scores beside it.
fn row_json(row: &RepoRow) -> Value {
    json!({
        "owner": row.repo_ref.owner,
        "name": row.repo_ref.name,
        "description": row.description,
        "stars": row.stars,
        "language": row.primary_language,
        "license": row.license_name,
        "topics": row.topics,
        "tags": row.tags,
        "created_at": row.created_at,
        "last_commit_at": row.last_commit_at,
        "starred_at": row.starred_at,
        "health_score": row.health.total,
        "excerpt": row.excerpt,
    })
}

fn get_repository(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let rowid = resolve_rowid(ctx.store, &owner, &name)?;
    let detail = ctx
        .store
        .repo_detail(rowid as u64)
        .map_err(|error| format!("catalog: {error}"))?
        .ok_or_else(|| format!("the catalog has no {owner}/{name}"))?;
    let fit = ctx
        .store
        .fit_score_of(rowid)
        .map_err(|error| format!("catalog: {error}"))?;
    let tags = ctx
        .store
        .tag_names(rowid)
        .map_err(|error| format!("catalog: {error}"))?;
    let note = ctx
        .store
        .note_of(NoteSubject::Repo(rowid))
        .map_err(|error| format!("catalog: {error}"))?;
    let readme_stored = ctx
        .store
        .readme_body(rowid as u64)
        .map_err(|error| format!("catalog: {error}"))?
        .is_some();
    let missing: Vec<&str> = ctx
        .store
        .missing_kinds(rowid as u64)
        .map_err(|error| format!("catalog: {error}"))?
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    Ok(json!({
        "repo": {
            "owner": detail.repo_ref.owner,
            "name": detail.repo_ref.name,
            "description": detail.description,
            "stars": detail.stars,
            "languages": detail.languages,
            "license": detail.license,
            "topics": detail.topics,
            "default_branch": detail.default_branch,
            "created_at": detail.created_at,
            "last_commit_at": detail.last_commit_at,
            "starred_at": detail.starred_at,
            "first_commit_date": detail.first_commit_date,
            "first_commit_source": detail.first_commit_source,
            "commit_count": detail.commit_count,
            "latest_release": detail.latest_release,
            "is_fork": detail.is_fork,
            "is_archived": detail.is_archived,
        },
        "health": detail.health,
        "fit": fit.map(|(_, report)| serde_json::from_str::<Value>(&report).unwrap_or(Value::Null)),
        "tags": tags,
        "note": note,
        "cached": { "readme": readme_stored, "missing": missing },
    }))
}

fn read_readme(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let rowid = resolve_rowid(ctx.store, &owner, &name)?;
    let body = ctx
        .store
        .readme_body(rowid as u64)
        .map_err(|error| format!("catalog: {error}"))?
        .ok_or_else(|| format!("no README is stored for {owner}/{name}"))?;
    Ok(json!({ "text": body.rewritten, "truncated": body.truncated }))
}

fn list_vocabulary(ctx: &mut ToolContext) -> Result<Value, String> {
    let tags = ctx
        .store
        .tags_with_counts(None)
        .map_err(|error| format!("catalog: {error}"))?;
    let topics = ctx
        .store
        .stats_top_topics(200)
        .map_err(|error| format!("catalog: {error}"))?;
    let tags: Vec<Value> = tags
        .iter()
        .map(|tag| json!({ "name": tag.name, "count": tag.repos }))
        .collect();
    let topics: Vec<Value> = topics
        .iter()
        .map(|topic| json!({ "name": topic.name, "count": topic.count }))
        .collect();
    Ok(json!({ "tags": tags, "topics": topics }))
}

fn list_local_repositories(ctx: &mut ToolContext) -> Result<Value, String> {
    let repos = ctx
        .store
        .local_repos()
        .map_err(|error| format!("catalog: {error}"))?;
    let entries: Vec<Value> = repos
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "name": entry.name,
                "path": entry.path,
                "root": entry.root,
                "owner": entry.owner,
                "repo_name": entry.repo_name,
                "origin": entry.origin,
                "branch": entry.branch,
                "missing": entry.missing_at.is_some(),
            })
        })
        .collect();
    Ok(json!({ "repositories": entries }))
}

fn read_local_file(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let id = arguments
        .get("repo_id")
        .and_then(Value::as_i64)
        .ok_or("`repo_id` is required and must be a number")?;
    let relative = str_arg(arguments, "path")?;
    let entry = ctx
        .store
        .local_repo(id)
        .map_err(|error| format!("catalog: {error}"))?
        .ok_or_else(|| format!("no local repository with id {id} — see list_local_repositories"))?;
    let preview = yagna_store::read_preview(std::path::Path::new(&entry.root), relative)
        .map_err(|error| format!("{error}"))?;
    Ok(json!({
        "text": preview.text,
        "truncated": preview.truncated,
        "binary": preview.binary,
        "bytes": preview.bytes,
    }))
}

// --- The fit spec.

fn get_fit_spec(ctx: &mut ToolContext) -> Result<Value, String> {
    let path = ctx.data_dir.join("fit.json");
    let (spec, loud_line) = fitspec::load_from(&path);
    let source = if path.exists() { "file" } else { "default" };
    let catalogue: Vec<Value> = yagna_core::fit::PARAMETERS
        .iter()
        .map(|name| {
            let kind = match yagna_core::fit::parameter_kind(name) {
                Some(ParamKind::Number) => "number",
                Some(ParamKind::Category) => "category",
                Some(ParamKind::Set) => "set",
                Some(ParamKind::Boolean) => "boolean",
                None => "number",
            };
            json!({ "name": name, "kind": kind })
        })
        .collect();
    Ok(json!({
        "spec": spec,
        "source": source,
        "loud_line": loud_line,
        "catalogue": catalogue,
    }))
}

fn write_fit_spec(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let text = str_arg(arguments, "spec")?;
    let spec =
        yagna_core::fit::parse_spec(text).map_err(|error| format!("fit spec refused: {error}"))?;
    let path = ctx.data_dir.join("fit.json");
    let before = fitspec::load_from(&path).0;
    fitspec::save_to(&path, &spec)?;
    let changed = fitspec::changed_parameters(&before, &spec);
    Ok(json!({ "changed": changed, "version": spec.version }))
}

fn rescore(ctx: &mut ToolContext) -> Result<Value, String> {
    let (spec, _) = fitspec::load_from(&ctx.data_dir.join("fit.json"));
    let now = Utc::now();
    let report = ctx
        .store
        .rescore_fit(&spec, now)
        .map_err(|error| format!("catalog: {error}"))?;
    let at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    ctx.store
        .set_app_state(yagna_store::FIT_RESCORED_AT_KEY, &at)
        .map_err(|error| format!("catalog: {error}"))?;
    ctx.store
        .set_app_state(
            yagna_store::FIT_RESCORED_MOVED_KEY,
            &report.moved.to_string(),
        )
        .map_err(|error| format!("catalog: {error}"))?;
    ctx.store
        .set_app_state(
            yagna_store::FIT_RESCORED_ROWS_KEY,
            &report.scored.to_string(),
        )
        .map_err(|error| format!("catalog: {error}"))?;
    Ok(json!({ "scored": report.scored, "moved": report.moved }))
}

// --- The writes.

fn set_tag(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let tag = str_arg(arguments, "tag")?;
    let attach = arguments
        .get("attach")
        .and_then(Value::as_bool)
        .ok_or("`attach` is required and must be true or false")?;
    let rowid = resolve_rowid(ctx.store, &owner, &name)?;
    if attach {
        ctx.store
            .assign_tag(rowid as u64, tag, Some(TagKind::Tag))
            .map_err(|error| format!("catalog: {error}"))?;
    } else {
        let tag_id = ctx
            .store
            .tag_id_by_name(tag)
            .map_err(|error| format!("catalog: {error}"))?;
        if let Some(tag_id) = tag_id {
            ctx.store
                .unassign_tag(rowid as u64, tag_id)
                .map_err(|error| format!("catalog: {error}"))?;
        }
    }
    Ok(json!({
        "repo": format!("{owner}/{name}"),
        "tag": tag,
        "attached": attach,
    }))
}

fn set_note(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let note = arguments
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let rowid = resolve_rowid(ctx.store, &owner, &name)?;
    ctx.store
        .set_note(NoteSubject::Repo(rowid), note, Utc::now())
        .map_err(|error| format!("catalog: {error}"))?;
    let stored = if note.trim().is_empty() {
        Value::Null
    } else {
        json!(note.trim())
    };
    Ok(json!({ "repo": format!("{owner}/{name}"), "note": stored }))
}

// --- The live tools.

fn search_github(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let query = discover_arguments(arguments)?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50) as usize;
    let token = require_token(ctx)?;
    let host = yagna_forge::ForgeHost::new().map_err(|error| format!("async runtime: {error}"))?;
    let forge = host
        .forge(|| yagna_forge::GithubForge::new(&token))
        .map_err(|error| error.describe())?;
    let found = host
        .search(&forge, &query, Utc::now())
        .map_err(|error| error.describe())?;
    let results: Vec<Value> = found.iter().take(limit).map(snapshot_json).collect();
    Ok(json!({ "query": query, "results": results }))
}

fn get_github_repository(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let token = require_token(ctx)?;
    let host = yagna_forge::ForgeHost::new().map_err(|error| format!("async runtime: {error}"))?;
    let forge = host
        .forge(|| yagna_forge::GithubForge::new(&token))
        .map_err(|error| error.describe())?;
    let facts = host
        .block_on(forge.repository(&repo_ref(&owner, &name)))
        .map_err(|error| error.describe())?;
    Ok(live_facts_json(&facts, &owner, &name))
}

fn refresh_repository(arguments: &Value, ctx: &mut ToolContext) -> Result<Value, String> {
    let (owner, name) = repo_args(arguments)?;
    let token = require_token(ctx)?;
    let rowid = resolve_rowid(ctx.store, &owner, &name)?;
    let host = yagna_forge::ForgeHost::new().map_err(|error| format!("async runtime: {error}"))?;
    let forge = host
        .forge(|| yagna_forge::GithubForge::new(&token))
        .map_err(|error| error.describe())?;
    let facts = host
        .block_on(forge.repository(&repo_ref(&owner, &name)))
        .map_err(|error| error.describe())?;
    // The reader's star time is theirs, not a live fact: the refresh
    // carries the stored one onto the snapshot before the upsert.
    let mut snapshot = facts.snapshot;
    snapshot.starred_at = ctx
        .store
        .starred_at_of(rowid)
        .map_err(|error| format!("catalog: {error}"))?;
    let now = Utc::now();
    ctx.store
        .upsert_snapshots(&[snapshot], now)
        .map_err(|error| format!("catalog: {error}"))?;
    let (spec, _) = fitspec::load_from(&ctx.data_dir.join("fit.json"));
    let moved = ctx
        .store
        .rescore_fit_row(rowid, &spec, now)
        .map_err(|error| format!("catalog: {error}"))?;
    let fit = ctx
        .store
        .fit_score_of(rowid)
        .map_err(|error| format!("catalog: {error}"))?;
    Ok(json!({
        "repo": format!("{owner}/{name}"),
        "refreshed": true,
        "open_issues": facts.open_issues,
        "fit_score": fit.map(|(total, _)| total),
        "fit_moved": moved > 0,
    }))
}

fn sync_catalog(ctx: &mut ToolContext) -> Result<Value, String> {
    let token = require_token(ctx)?;
    if ctx.sweeping.swap(true, Ordering::Relaxed) {
        return Err("a sweep is already running — this one is refused".to_string());
    }
    let budget = Arc::clone(&ctx.budget);
    let trace = Arc::clone(&ctx.trace);
    let sweeping = Arc::clone(&ctx.sweeping);
    std::thread::spawn(move || {
        let stop = AtomicBool::new(false);
        let outcome = crate::sync::perform_stoppable(
            &token,
            false,
            |line| {
                append_action(&trace, "sync_catalog", &line, ActionOutcome::Ok);
            },
            &stop,
            || charge_page(&budget),
        );
        match outcome {
            Ok((summary, _)) => append_action(&trace, "sync_catalog", &summary, ActionOutcome::Ok),
            Err(problem) => append_action(&trace, "sync_catalog", &problem, ActionOutcome::Error),
        }
        sweeping.store(false, Ordering::Relaxed);
    });
    Ok(json!({ "enqueued": true }))
}

/// The sweep's per-page spend gate: one core call's charge against the
/// session ledger, refused when the ledger refuses.
fn charge_page(budget: &Mutex<Budget>) -> Result<(), String> {
    let mut ledger = budget
        .lock()
        .map_err(|_| "the spend ledger is unavailable".to_string())?;
    match ledger.admit(SpendKind::Core, Utc::now()) {
        Admission::Admitted => Ok(()),
        Admission::Confirm { message } | Admission::Refused { message } => Err(message),
    }
}

fn append_action(trace: &Mutex<Trace>, tool: &str, subject: &str, outcome: ActionOutcome) {
    if let Ok(mut trace) = trace.lock() {
        let _ = trace.append(&TraceLine::action(
            Utc::now(),
            tool,
            subject,
            outcome,
            Some(SpendKind::Core),
        ));
    }
}

fn repo_ref(owner: &str, name: &str) -> RepoRef {
    RepoRef {
        forge: yagna_core::model::Forge::GitHub,
        owner: owner.to_string(),
        name: name.to_string(),
    }
}

fn snapshot_json(snapshot: &yagna_core::model::RepoSnapshot) -> Value {
    json!({
        "owner": snapshot.owner,
        "name": snapshot.name,
        "description": snapshot.description,
        "stars": snapshot.stars,
        "language": snapshot.languages.first().map(|l| l.name.clone()),
        "license": snapshot.license,
        "topics": snapshot.topics,
        "default_branch": snapshot.default_branch,
        "created_at": snapshot.created_at,
        "last_commit_at": snapshot.last_commit_at,
        "latest_release": snapshot.latest_release,
        "is_fork": snapshot.is_fork,
        "is_archived": snapshot.is_archived,
    })
}

fn live_facts_json(facts: &RepoLiveFacts, owner: &str, name: &str) -> Value {
    let mut value = snapshot_json(&facts.snapshot);
    value["owner"] = json!(owner);
    value["name"] = json!(name);
    value["open_issues"] = json!(facts.open_issues);
    value["commit_count"] = json!(facts.snapshot.commit_count);
    value
}

/// The DISCOVER query text: the four tokens `created:<days>`,
/// `stars:<min>`, `language:<name>` and `sort:stars|newest`. Anything
/// else is refused with the supported set named.
pub fn discover_from_text(text: &str) -> Result<DiscoverQuery, String> {
    let mut query = DiscoverQuery::default();
    for token in text.split_whitespace() {
        let Some((key, value)) = token.split_once(':') else {
            return Err(unsupported_token(token));
        };
        match key {
            "created" => {
                query.created_within_days = value
                    .parse()
                    .map_err(|_| format!("`created:` wants a number of days, got {value:?}"))?;
            }
            "stars" => {
                query.min_stars = value
                    .parse()
                    .map_err(|_| format!("`stars:` wants a number, got {value:?}"))?;
            }
            "language" => {
                query.language = Some(value.to_string());
            }
            "sort" => {
                query.sort = match value {
                    "stars" => DiscoverSort::Stars,
                    "newest" => DiscoverSort::Newest,
                    other => {
                        return Err(format!("`sort:` wants `stars` or `newest`, got {other:?}"));
                    }
                };
            }
            other => return Err(unsupported_token(&format!("{other}:{value}"))),
        }
    }
    Ok(query)
}

fn unsupported_token(token: &str) -> String {
    format!(
        "unsupported term {token:?} — the DISCOVER query takes created:<days>, stars:<min>, language:<name>, sort:stars|newest"
    )
}

/// The live query the tool's arguments describe: the DISCOVER text's
/// tokens, plus the `word` and `topic` arguments on top. The word and
/// the topic ride as their own arguments — the agent hands over the
/// values, and the core's quoting rule, the same one the reader's
/// surface applies, decides what GitHub sees.
fn discover_arguments(arguments: &Value) -> Result<DiscoverQuery, String> {
    let mut query = discover_from_text(str_arg(arguments, "query")?)?;
    if let Some(word) = opt_str_arg(arguments, "word") {
        query.word = Some(word.to_string());
    }
    if let Some(topic) = opt_str_arg(arguments, "topic") {
        query.topic = Some(topic.to_string());
    }
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("yagna-tools-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _scratch: Scratch,
        store: Store,
        data_dir: PathBuf,
        token: Option<String>,
        budget: Arc<Mutex<Budget>>,
        trace: Arc<Mutex<Trace>>,
        sweeping: Arc<AtomicBool>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let scratch = Scratch::new(label);
            let data_dir = scratch.path();
            let store = Store::open(&data_dir.join("catalog.db")).expect("open");
            let (policy, _) = load_policy(&data_dir.join("agent.toml"));
            let trace = Trace::open(&data_dir.join(TRACE_FILE)).expect("trace");
            let ledger = Budget::new(policy, data_dir.join("agent.toml"));
            Self {
                _scratch: scratch,
                store,
                data_dir,
                token: Some("test-token".to_string()),
                budget: Arc::new(Mutex::new(ledger)),
                trace: Arc::new(Mutex::new(trace)),
                sweeping: Arc::new(AtomicBool::new(false)),
            }
        }

        fn ctx(&mut self) -> ToolContext<'_> {
            ToolContext {
                store: &mut self.store,
                token: self.token.clone(),
                data_dir: self.data_dir.clone(),
                budget: Arc::clone(&self.budget),
                trace: Arc::clone(&self.trace),
                sweeping: Arc::clone(&self.sweeping),
            }
        }

        fn seed(&mut self, owner: &str, name: &str, stars: u64) {
            let now = chrono::Utc::now();
            self.store
                .upsert_snapshots(&[snapshot(owner, name, stars)], now)
                .expect("upsert");
        }
    }

    fn snapshot(owner: &str, name: &str, stars: u64) -> yagna_core::model::RepoSnapshot {
        use chrono::TimeZone;
        use yagna_core::model::{Forge, LanguageStat, LicenseInfo};
        let at = |year: i32, month: u32, day: u32| {
            chrono::Utc
                .with_ymd_and_hms(year, month, day, 12, 0, 0)
                .single()
                .expect("date")
        };
        yagna_core::model::RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: owner.to_string(),
            name: name.to_string(),
            description: Some(format!("{name} is a fixture")),
            stars,
            languages: vec![LanguageStat {
                name: "Rust".to_string(),
                bytes: 1_000,
                color: None,
            }],
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: vec!["fixtures".to_string()],
            default_branch: Some("main".to_string()),
            created_at: at(2024, 1, 1),
            last_commit_at: at(2026, 10, 1),
            starred_at: Some(at(2026, 10, 20)),
            commit_count: 100,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    #[test]
    fn search_library_uses_the_grammar_when_the_query_names_a_field() {
        let mut fx = Fixture::new("grammar");
        fx.seed("owner", "sample-one", 10);
        fx.seed("owner", "sample-two", 500);
        let now = chrono::Utc::now();
        let rowid = fx
            .store
            .repo_rowid("github", "owner", "sample-two")
            .expect("rowid")
            .expect("row");
        fx.store
            .set_note(NoteSubject::Repo(rowid), "the big one", now)
            .expect("note");

        let result = execute(
            "search_library",
            &json!({"query": "stars:>100", "limit": 10}),
            &mut fx.ctx(),
        )
        .expect("search");
        let body = result;
        assert_eq!(body["mode"], "grammar");
        let hits = body["results"].as_array().expect("results");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["name"], "sample-two");
        assert_eq!(
            hits[0]["note"], "the big one",
            "the reader's note rides along"
        );
    }

    #[test]
    fn search_library_falls_back_to_fuzzy_for_plain_names() {
        let mut fx = Fixture::new("fuzzy");
        fx.seed("owner", "sample-one", 10);

        let result =
            execute("search_library", &json!({"query": "sampl"}), &mut fx.ctx()).expect("search");
        let body = result;
        assert_eq!(body["mode"], "fuzzy");
        let hits = body["results"].as_array().expect("results");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["name"], "sample-one");
    }

    #[test]
    fn get_repository_reports_the_whole_record() {
        let mut fx = Fixture::new("record");
        fx.seed("owner", "sample-one", 10);

        let result = execute(
            "get_repository",
            &json!({"forge": "github", "owner": "owner", "name": "sample-one"}),
            &mut fx.ctx(),
        )
        .expect("get");
        let body = result;
        assert_eq!(body["repo"]["name"], "sample-one");
        let health = body["health"]["total"].as_u64().expect("health total");
        assert!(health <= 12, "the pinned 12-point health index: {health}");
        assert!(
            !body["repo"]["topics"]
                .as_array()
                .expect("topics")
                .is_empty()
        );
    }

    /// The schemas publish owner and name and nothing else — a
    /// schema-faithful client sends no forge, and the repository
    /// tools must answer anyway.
    #[test]
    fn repository_tools_answer_without_a_forge_argument() {
        let mut fx = Fixture::new("no-forge");
        fx.seed("owner", "sample-one", 10);

        let result = execute(
            "get_repository",
            &json!({"owner": "owner", "name": "sample-one"}),
            &mut fx.ctx(),
        )
        .expect("the schema declares owner/name only");
        assert_eq!(result["repo"]["name"], "sample-one");
    }

    /// A forge that is sent is still validated: the typo guard stays.
    #[test]
    fn an_unknown_forge_is_still_refused() {
        let mut fx = Fixture::new("bad-forge");
        fx.seed("owner", "sample-one", 10);

        let error = execute(
            "get_repository",
            &json!({"forge": "gitlab", "owner": "owner", "name": "sample-one"}),
            &mut fx.ctx(),
        )
        .expect_err("the typo guard");
        assert!(error.contains("github"), "names the forge: {error}");
    }

    #[test]
    fn get_repository_names_a_repository_that_is_not_there() {
        let mut fx = Fixture::new("record-missing");

        let error = execute(
            "get_repository",
            &json!({"forge": "github", "owner": "nope", "name": "nothing"}),
            &mut fx.ctx(),
        )
        .expect_err("not in the catalog");
        assert!(
            error.contains("nope/nothing"),
            "the error names the repo: {error}"
        );
    }

    #[test]
    fn read_readme_returns_the_stored_body() {
        let mut fx = Fixture::new("readme");
        fx.seed("owner", "sample-one", 10);
        let now = chrono::Utc::now();
        let rowid = fx
            .store
            .repo_rowid("github", "owner", "sample-one")
            .expect("rowid")
            .expect("row");
        fx.store
            .store_readme(rowid as u64, "# Sample one\n\nHello.", now)
            .expect("store readme");

        let result = execute(
            "read_readme",
            &json!({"forge": "github", "owner": "owner", "name": "sample-one"}),
            &mut fx.ctx(),
        )
        .expect("readme");
        let body = result;
        assert!(body["text"].as_str().expect("text").contains("Hello"));
    }

    #[test]
    fn list_vocabulary_lists_tags_and_topics() {
        let mut fx = Fixture::new("vocabulary");
        fx.seed("owner", "sample-one", 10);
        let rowid = fx
            .store
            .repo_rowid("github", "owner", "sample-one")
            .expect("rowid")
            .expect("row");
        fx.store
            .assign_tag(rowid as u64, "favorites", Some(TagKind::Tag))
            .expect("tag");

        let result = execute("list_vocabulary", &json!({}), &mut fx.ctx()).expect("vocabulary");
        let body = result;
        assert_eq!(body["tags"][0]["name"], "favorites");
        assert_eq!(body["topics"][0]["name"], "fixtures");
    }

    #[test]
    fn set_tag_attaches_and_detaches_idempotently() {
        let mut fx = Fixture::new("tags");
        fx.seed("owner", "sample-one", 10);

        let arguments = json!({"forge": "github", "owner": "owner", "name": "sample-one", "tag": "later", "attach": true});
        execute("set_tag", &arguments, &mut fx.ctx()).expect("attach");
        execute("set_tag", &arguments, &mut fx.ctx()).expect("attach again");
        let rowid = fx
            .store
            .repo_rowid("github", "owner", "sample-one")
            .expect("rowid")
            .expect("row");
        assert!(
            fx.store
                .tag_names(rowid)
                .expect("tags")
                .contains(&"later".to_string())
        );
        let detach = json!({"forge": "github", "owner": "owner", "name": "sample-one", "tag": "later", "attach": false});
        execute("set_tag", &detach, &mut fx.ctx()).expect("detach");
        execute("set_tag", &detach, &mut fx.ctx()).expect("detach again");
        assert!(fx.store.tag_names(rowid).expect("tags").is_empty());
    }

    #[test]
    fn set_note_writes_reads_and_deletes() {
        let mut fx = Fixture::new("notes");
        fx.seed("owner", "sample-one", 10);

        let write =
            json!({"forge": "github", "owner": "owner", "name": "sample-one", "note": "read this"});
        execute("set_note", &write, &mut fx.ctx()).expect("note");
        let rowid = fx
            .store
            .repo_rowid("github", "owner", "sample-one")
            .expect("rowid")
            .expect("row");
        let note = fx.store.note_of(NoteSubject::Repo(rowid)).expect("note");
        assert!(note.expect("note").contains("read this"));
        let clear = json!({"forge": "github", "owner": "owner", "name": "sample-one", "note": ""});
        execute("set_note", &clear, &mut fx.ctx()).expect("clear");
        let note = fx.store.note_of(NoteSubject::Repo(rowid)).expect("note");
        assert!(note.is_none(), "an empty note deletes");
    }

    #[test]
    fn get_fit_spec_returns_the_file_the_default_and_the_catalogue() {
        let mut fx = Fixture::new("fit-spec");

        let result = execute("get_fit_spec", &json!({}), &mut fx.ctx()).expect("spec");
        let body = result;
        assert_eq!(body["source"], "default");
        assert_eq!(body["catalogue"].as_array().expect("catalogue").len(), 18);
        assert_eq!(body["catalogue"][0]["kind"], "number");
    }

    #[test]
    fn write_fit_spec_validates_backs_up_and_counts_the_changes() {
        let mut fx = Fixture::new("write-spec");

        let default_text = serde_json::to_string(&yagna_core::fit::default_spec()).expect("text");
        let first = execute(
            "write_fit_spec",
            &json!({"spec": default_text}),
            &mut fx.ctx(),
        )
        .expect("write");
        let body = first;
        assert_eq!(body["changed"].as_array().expect("changed").len(), 0);
        let mut changed = yagna_core::fit::default_spec();
        changed.parameters[0].weight += 1.0;
        let changed_text = serde_json::to_string(&changed).expect("text");
        let second = execute(
            "write_fit_spec",
            &json!({"spec": changed_text}),
            &mut fx.ctx(),
        )
        .expect("write");
        let body = second;
        assert_eq!(body["changed"], json!(["recency_days"]));
        assert!(
            fx.data_dir.join("fit.previous.json").exists(),
            "the replaced spec is backed up"
        );
        let bad = execute(
            "write_fit_spec",
            &json!({"spec": r#"{"version":1,"name":"bad","parameters":[{"name":"shell","weight":1,"when_true":1}]}"#}),
            &mut fx.ctx(),
        )
        .expect_err("refused");
        assert!(
            bad.contains("catalogue"),
            "the error names the catalogue: {bad}"
        );
    }

    #[test]
    fn rescore_scores_the_catalog_and_records_the_counts() {
        let mut fx = Fixture::new("rescore");
        fx.seed("owner", "sample-one", 10);
        fx.seed("owner", "sample-two", 20);

        let result = execute("rescore", &json!({}), &mut fx.ctx()).expect("rescore");
        let body = result;
        assert_eq!(body["scored"], 2);
        assert_eq!(body["moved"], 2);
        let moved = fx
            .store
            .app_state(yagna_store::FIT_RESCORED_MOVED_KEY)
            .expect("state");
        assert_eq!(moved.as_deref(), Some("2"));
    }

    #[test]
    fn the_discover_text_parses_the_four_tokens() {
        let parsed =
            discover_from_text("created:30 stars:200 language:rust sort:newest").expect("parsed");
        assert_eq!(parsed.created_within_days, 30);
        assert_eq!(parsed.min_stars, 200);
        assert_eq!(parsed.language.as_deref(), Some("rust"));
        assert_eq!(parsed.sort, yagna_core::model::DiscoverSort::Newest);
        let error = discover_from_text("word:thing").expect_err("unknown token");
        assert!(
            error.contains("created:"),
            "the error names the tokens: {error}"
        );
    }

    #[test]
    fn the_live_search_takes_the_word_and_the_topic_arguments() {
        let arguments = json!({
            "query": "created:30 stars:200",
            "word": "web server",
            "topic": "wasm"
        });
        let query = discover_arguments(&arguments).expect("parsed");
        assert_eq!(query.word.as_deref(), Some("web server"));
        assert_eq!(query.topic.as_deref(), Some("wasm"));
        // The word reaches GitHub through the core's quoting rule — the
        // agent never quotes by hand.
        let text = query.query_text(chrono::Utc::now());
        assert!(
            text.contains("\"web server\""),
            "the word rides quoted: {text}"
        );
        let bare = discover_arguments(&json!({ "query": "stars:10" })).expect("parsed");
        assert_eq!(bare.word, None, "no word argument, no word");
        let blank =
            discover_arguments(&json!({ "query": "stars:10", "word": "   " })).expect("parsed");
        assert_eq!(blank.word, None, "a blank word rides as absent");
        let error = discover_arguments(&json!({ "word": "thing" })).expect_err("no query");
        assert!(error.contains("query"), "the error names the fix: {error}");
    }

    #[test]
    fn the_live_tools_need_a_token_before_anything_else() {
        let mut fx = Fixture::new("live-token");
        fx.token = None;
        fx.seed("owner", "sample-one", 10);

        for (tool, arguments) in [
            ("search_github", json!({"query": "stars:10"})),
            (
                "get_github_repository",
                json!({"forge": "github", "owner": "owner", "name": "sample-one"}),
            ),
            (
                "refresh_repository",
                json!({"forge": "github", "owner": "owner", "name": "sample-one"}),
            ),
            ("sync_catalog", json!({})),
        ] {
            let error = execute(tool, &arguments, &mut fx.ctx()).expect_err("no token");
            assert!(error.contains("token"), "{tool}: {error}");
        }
    }

    #[test]
    fn refresh_repository_needs_the_row_in_the_catalog() {
        let mut fx = Fixture::new("refresh-missing");

        let error = execute(
            "refresh_repository",
            &json!({"forge": "github", "owner": "nope", "name": "nothing"}),
            &mut fx.ctx(),
        )
        .expect_err("not in the catalog");
        assert!(
            error.contains("nope/nothing"),
            "the error names the repo: {error}"
        );
    }

    #[test]
    fn sync_catalog_refuses_a_second_sweep() {
        let mut fx = Fixture::new("sweep-twice");
        fx.sweeping.store(true, Ordering::Relaxed);

        let error =
            execute("sync_catalog", &json!({}), &mut fx.ctx()).expect_err("already running");
        assert!(error.contains("already running"), "{error}");
    }
}
