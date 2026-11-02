//! The `sync` verb: one full starred sweep, upserted into the catalog
//! page by page, summarized in one line — with an optional `--enrich`
//! phase that works the lazy layer's queue afterwards.
//!
//! Pages arrive through the forge's sweep job and are each stored in one
//! transaction — a killed run loses at most one page, and nothing is
//! marked missing unless the sweep ran to the end (a failed sweep saw
//! too little to judge absence by). The rate budget is read before the
//! burst (the free `/rate_limit` probe) and rides on every page (the
//! GraphQL `rateLimit` field); under the floor the sweep pauses until
//! the reset, visibly, through the same progress sink the UI renders.
//! Quiet mode prints nothing until the summary; `--verbose` adds one
//! line per page. A 401/403 surfaces as the token problem it is, never
//! a panic, and the ledger row keeps the same text for the record. The
//! enrich phase runs whatever the sweep did — the queue is independent
//! of the sweep's outcome. `perform` is the same pipeline the app's
//! update worker runs — one sweep, one summary, wherever it is driven
//! from — and `perform_stoppable` is the worker's own way in: the
//! reader's stop is polled at every page boundary and lands as a
//! terminal state of its own.
//!
//! Items: `run` — the verb; `perform`/`perform_stoppable` — the
//! sweep-to-summary pipeline (`spend_gate` refuses a page at its
//! boundary); `SweepEnd` — how a sweep ended; `SweepRun`/`SweepStep` —
//! the page loop; `store_page` — one page's upsert; `Tally` — what the
//! summary line reports; `human_duration` — the summary's clock.
//! Depends on: yagna_store (the catalog), yagna_forge (the sweep),
//! crate::enrich, crate::pacing, crate::token, crate::paths,
//! crate::fitspec (the end-of-sweep fit re-score).
//! Called by: crate::main (the `sync` verb), crate::updater (the
//! app's worker) and crate::mcp::tools (the `sync_catalog` sweep).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use yagna_forge::{ForgeHost, GithubForge, SweepJob, SweepPage};
use yagna_store::{KIND_FULL, Store, SyncRunRecord};

/// Runs the sweep (and, when asked, the enrichment) and prints the
/// summaries — or the failure.
pub(crate) fn run(verbose: bool, enrich: bool) -> ExitCode {
    let Some(token) = crate::token::acquire(verbose) else {
        println!("sync: no token — set GITHUB_TOKEN or run `gh auth login`");
        return ExitCode::FAILURE;
    };
    let swept = perform(&token, verbose, |line| println!("{line}"));
    let mut sweep_ok = swept.is_ok();
    if let Err(problem) = &swept {
        println!("sync failed: {problem}");
    }
    if enrich {
        sweep_ok &= enrich_after(&token, verbose);
    }
    if sweep_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// How a sweep ended when it ended on its own terms: it ran to the end,
/// or the reader stopped it at a page boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepEnd {
    /// The sweep ran to the end: the missing-mark applies.
    Finished,
    /// The reader stopped the sweep: no missing-mark — a stopped sweep
    /// saw too little to judge absence by, exactly like a failed one.
    Stopped,
}

/// The pipeline with no stop flag and no spend gate — the CLI verb's
/// way in.
pub(crate) fn perform(
    token: &str,
    verbose: bool,
    progress: impl FnMut(String),
) -> Result<String, String> {
    let unstopped = AtomicBool::new(false);
    perform_stoppable(token, verbose, progress, &unstopped, || Ok(())).map(|(summary, _)| summary)
}

/// The pipeline: open the catalog, sweep, store every page as it lands,
/// close the books. Progress lines go to the caller's sink (the CLI
/// prints them; the app's worker sends them over its channel); the
/// return is the summary line, or the problem. The missing-mark and the
/// ledger row only happen when the sweep ran to the end — a failed or
/// stopped sweep saw too little to judge absence by, and its ledger row
/// records what did happen instead. `stop` is polled at every page
/// boundary: a sweep the reader stops finishes the page it is on and
/// lands as its own terminal state, never mid-page. `spend_gate` runs
/// before every page's API call: an `Err` stops the sweep at the page
/// boundary with that message (the agent's spend policy refusing a
/// page), the same terminal shape as a failure but with its own words.
pub(crate) fn perform_stoppable(
    token: &str,
    verbose: bool,
    mut progress: impl FnMut(String),
    stop: &AtomicBool,
    mut spend_gate: impl FnMut() -> Result<(), String>,
) -> Result<(String, SweepEnd), String> {
    let mut store = crate::paths::open_catalog()?;
    let host = ForgeHost::new().map_err(|error| format!("async runtime: {error}"))?;
    crate::pacing::ensure_headroom(&host, token, &mut progress);
    let started = Utc::now();
    let run = SweepRun {
        store: &mut store,
        started,
        verbose,
        tally: Tally::default(),
        sink: &mut progress,
        stop,
        spend_gate: &mut spend_gate,
    };
    let (tally, problem, stopped) = run.drive(&host, token);
    let kind = KIND_FULL.to_string();
    let outcome = match problem {
        Some(problem) => {
            record_failure(&mut store, started, &tally, &kind, &problem, &mut progress)
        }
        None if stopped => {
            let finished = Utc::now();
            let summary = format!(
                "stopped after {} — {} pages, {} repos seen",
                human_duration(finished - started),
                tally.pages,
                tally.seen
            );
            let record = SyncRunRecord {
                started_at: started,
                finished_at: Some(finished),
                kind,
                pages: Some(tally.pages),
                repos_seen: tally.seen,
                repos_upserted: tally.upserted,
                repos_new: 0,
                repos_missing: 0,
                error: None,
            };
            store
                .record_sync_run(&record)
                .map_err(|error| format!("catalog: {error}"))?;
            progress(summary.clone());
            Ok((summary, SweepEnd::Stopped))
        }
        None => {
            let finished = Utc::now();
            let missing = store
                .mark_missing(&tally.seen_identities, finished)
                .map_err(|error| format!("catalog: {error}"))?;
            let fresh = store
                .count_first_seen_at(started)
                .map_err(|error| format!("catalog: {error}"))?;
            // A fresh row carries its health from the upsert and its fit
            // from here: one sweep's end, both scores current.
            let (fit_spec, _) = crate::fitspec::load();
            let fit = store
                .rescore_fit(&fit_spec, finished)
                .map_err(|error| format!("catalog: {error}"))?;
            let record = SyncRunRecord {
                started_at: started,
                finished_at: Some(finished),
                kind,
                pages: Some(tally.pages),
                repos_seen: tally.seen,
                repos_upserted: tally.upserted,
                repos_new: fresh,
                repos_missing: missing,
                error: None,
            };
            store
                .record_sync_run(&record)
                .map_err(|error| format!("catalog: {error}"))?;
            let summary = format!(
                "sync: seen {}, upserted {}, new {fresh}, missing {missing}, health recomputed, fit re-scored {} rows ({})",
                tally.seen,
                tally.upserted,
                fit.scored,
                human_duration(finished - started),
            );
            progress(summary.clone());
            Ok((summary, SweepEnd::Finished))
        }
    };
    let _ = store.checkpoint_wal();
    outcome
}

/// Records a failed sweep's ledger row and shapes its error.
fn record_failure(
    store: &mut Store,
    started: DateTime<Utc>,
    tally: &Tally,
    kind: &str,
    problem: &str,
    progress: &mut impl FnMut(String),
) -> Result<(String, SweepEnd), String> {
    let record = SyncRunRecord {
        started_at: started,
        finished_at: None,
        kind: kind.to_string(),
        pages: Some(tally.pages),
        repos_seen: tally.seen,
        repos_upserted: tally.upserted,
        repos_new: 0,
        repos_missing: 0,
        error: Some(problem.to_string()),
    };
    match store.record_sync_run(&record) {
        Ok(()) => {
            let line = format!("sync failed: {problem}");
            progress(line.clone());
            Err(problem.to_string())
        }
        Err(error) => Err(format!(
            "{problem} (and the run could not be recorded: {error})"
        )),
    }
}

/// The enrichment phase, after the sweep: its own store handle and
/// host, the queue worked to emptiness.
fn enrich_after(token: &str, verbose: bool) -> bool {
    let mut store = match crate::paths::open_catalog() {
        Ok(store) => store,
        Err(problem) => {
            println!("enrich failed: {problem}");
            return false;
        }
    };
    let host = match ForgeHost::new() {
        Ok(host) => host,
        Err(error) => {
            println!("enrich failed: async runtime: {error}");
            return false;
        }
    };
    crate::enrich::run(&mut store, &host, token, verbose) == ExitCode::SUCCESS
}

/// One sweep's working state — what every page's step runs against,
/// and the driver of the page loop itself. Every page lands through
/// `step`: the stop and the spend gate are polled at the page
/// boundary, never mid-page.
struct SweepRun<'a, S: FnMut(String), G: FnMut() -> Result<(), String>> {
    store: &'a mut Store,
    started: DateTime<Utc>,
    verbose: bool,
    tally: Tally,
    sink: &'a mut S,
    stop: &'a AtomicBool,
    spend_gate: &'a mut G,
}

/// One step of the sweep loop: the page boundary's gates, then the
/// page's own pull and store.
enum SweepStep {
    /// A page landed.
    Landed,
    /// The sweep has no pages left.
    Exhausted,
    /// The reader stopped the sweep.
    Stopped,
    /// The page failed or the spend gate refused it.
    Failed(String),
}

impl<S: FnMut(String), G: FnMut() -> Result<(), String>> SweepRun<'_, S, G> {
    /// Drives the sweep to its end — or to the reader's stop — storing
    /// every page as it lands. Returns the tally, the problem when the
    /// sweep failed or the gate refused a page, and whether the reader
    /// stopped it.
    fn drive(mut self, host: &ForgeHost, token: &str) -> (Tally, Option<String>, bool) {
        let token = token.to_string();
        let mut job = host.sweep(move || GithubForge::new(&token));
        let mut problem = None;
        let mut stopped = false;
        host.block_on(async {
            loop {
                match self.step(&mut job).await {
                    SweepStep::Landed => {}
                    SweepStep::Exhausted => break,
                    SweepStep::Stopped => {
                        stopped = true;
                        break;
                    }
                    SweepStep::Failed(message) => {
                        problem = Some(message);
                        break;
                    }
                }
            }
        });
        (self.tally, problem, stopped)
    }

    async fn step(&mut self, job: &mut SweepJob) -> SweepStep {
        if self.stop.load(Ordering::Relaxed) {
            return SweepStep::Stopped;
        }
        if let Err(message) = (self.spend_gate)() {
            return SweepStep::Failed(message);
        }
        let Some(page_result) = job.next_page().await else {
            return SweepStep::Exhausted;
        };
        match page_result
            .map_err(|error| error.describe())
            .and_then(|page| {
                take_page(
                    self.store,
                    &page,
                    self.started,
                    self.verbose,
                    &mut self.tally,
                    self.sink,
                )
            }) {
            Ok(()) => SweepStep::Landed,
            Err(problem) => SweepStep::Failed(problem),
        }
    }
}

/// One landed page: the headroom pause before it, the store, the
/// tally. `Err` is the sweep's problem — the caller stops the sweep.
fn take_page(
    store: &mut Store,
    page: &SweepPage,
    started: DateTime<Utc>,
    verbose: bool,
    tally: &mut Tally,
    sink: &mut impl FnMut(String),
) -> Result<(), String> {
    crate::pacing::ensure_page_headroom(page.rate_remaining, page.rate_reset, sink);
    store_page(store, page, started, verbose, tally, sink)
}

/// One page into the catalog: the page's transaction, then the running
/// totals. The seen identities accumulate for the missing-mark.
fn store_page(
    store: &mut Store,
    page: &SweepPage,
    started: DateTime<Utc>,
    verbose: bool,
    tally: &mut Tally,
    sink: &mut impl FnMut(String),
) -> Result<(), String> {
    let written = store
        .upsert_snapshots(&page.repos, started)
        .map_err(|error| format!("catalog: {error}"))?;
    tally.pages += 1;
    tally.seen += page.repos.len() as u64;
    tally.upserted += written;
    tally.seen_identities.extend(
        page.repos
            .iter()
            .map(|snapshot| (snapshot.owner.clone(), snapshot.name.clone())),
    );
    if verbose {
        match &page.cursor {
            Some(cursor) => sink(format!(
                "page {}: {} repos (cursor {cursor})",
                page.number,
                page.repos.len()
            )),
            None => sink(format!(
                "page {}: {} repos (last page)",
                page.number,
                page.repos.len()
            )),
        }
    }
    Ok(())
}

/// What the summary line reports: everything the sweep saw, everything
/// that was written, and the identities it covered.
#[derive(Default)]
struct Tally {
    pages: u64,
    seen: u64,
    upserted: u64,
    seen_identities: Vec<(String, String)>,
}

/// The summary's clock: hours, minutes and seconds — a unit only when it
/// carries weight.
fn human_duration(elapsed: chrono::Duration) -> String {
    let seconds = elapsed.num_seconds().max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let rest = seconds % 60;
    match (hours, minutes) {
        (0, 0) => format!("{rest}s"),
        (0, _) => format!("{minutes}m{rest}s"),
        (_, _) => format!("{hours}h{minutes}m"),
    }
}
