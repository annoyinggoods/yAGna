//! The `stats` verb: what the catalog holds, printed as lines — and
//! the maintenance view: the cache's bytes and evictions, the
//! database's footprint and its WAL, with the `VACUUM` guidance.
//!
//! Lines: the repo counts by presence and shape, the health spread,
//! the sync ledger's last word, the cache's per-kind sizes with its
//! eviction counters, and the database's footprint (file, WAL, pages,
//! freelist) with the vacuum suggestion when the freelist runs large.
//!
//! Items: `run` — the verb; `health_line` — the score spread
//! formatted; `cache_lines`/`database_lines` — the maintenance half.
//! Depends on: yagna_store (the catalog, the cache breakdown, the
//! footprint), crate::paths (the cache dir and `human_bytes`),
//! crate::settings (the cap).
//! Called by: crate::main (the `stats` verb).

use std::process::ExitCode;

use yagna_store::{CatalogOverview, DbFootprint, Store};

/// Reads the catalog and prints its counts and its maintenance view.
pub(crate) fn run() -> ExitCode {
    let mut store = match crate::paths::open_catalog() {
        Ok(store) => store,
        Err(problem) => {
            println!("stats failed: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let overview = match store.overview() {
        Ok(overview) => overview,
        Err(problem) => {
            println!("stats failed: catalog: {problem}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "stats: {} repos ({} present, {} missing, {} forks, {} archived)",
        overview.repos_total,
        overview.repos_present,
        overview.repos_missing,
        overview.forks,
        overview.archived,
    );
    println!("stats: health {}", health_line(&overview));
    match (store.count_runs(), store.latest_run()) {
        (Ok(count), Ok(Some(run))) => {
            let ended = run
                .finished_at
                .map(|at| at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_else(|| "unfinished".to_string());
            let failures = if run.error.is_some() { ", failed" } else { "" };
            println!(
                "stats: {count} sync run(s), last {} {ended} ({} pages, {} seen, {} upserted, {} new, {} missing{failures})",
                run.kind,
                run.pages.unwrap_or(0),
                run.repos_seen,
                run.repos_upserted,
                run.repos_new,
                run.repos_missing,
            );
        }
        (Ok(_), Ok(None)) => println!("stats: no sync run recorded yet"),
        (Err(problem), _) | (_, Err(problem)) => {
            println!("stats failed: catalog: {problem}");
            return ExitCode::FAILURE;
        }
    }
    let cache_exit = cache_lines(&store);
    let database_exit = database_lines(&mut store);
    // The read's closing move: the WAL's pages go back into the file.
    let _ = store.checkpoint_wal();
    if cache_exit == ExitCode::SUCCESS && database_exit == ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The cache's half: the bytes per kind, the cap, the eviction
/// counters — what the eviction policy has been doing.
fn cache_lines(store: &Store) -> ExitCode {
    let (settings, warning) = crate::settings::load();
    if let Some(line) = warning {
        println!("{line}");
    }
    let cache_dir = match crate::paths::cache() {
        Ok(dir) => dir,
        Err(problem) => {
            println!("stats failed: cache dir: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let on_disk = yagna_store::cache_stats(&cache_dir);
    let breakdown = match store.cache_breakdown() {
        Ok(breakdown) => breakdown,
        Err(problem) => {
            println!("stats failed: catalog: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let human = crate::paths::human_bytes;
    println!(
        "stats: cache {} in {} file(s) on disk — images {}, digests {} in {} take(s), cap {}",
        human(on_disk.bytes),
        on_disk.files,
        human(breakdown.asset_bytes),
        human(breakdown.digest_bytes),
        breakdown.digest_count,
        human(settings.cache_cap_bytes),
    );
    let evicted_files = store
        .app_state_counter(yagna_store::EVICTED_FILES_KEY)
        .unwrap_or(0);
    let evicted_bytes = store
        .app_state_counter(yagna_store::EVICTED_BYTES_KEY)
        .unwrap_or(0);
    println!(
        "stats: evictions so far {} object(s), {}",
        evicted_files,
        human(evicted_bytes),
    );
    ExitCode::SUCCESS
}

/// The database's half: the file, the WAL, the pages and the freelist,
/// the biggest objects — the maintenance view, with the vacuum
/// suggestion when the freelist runs past a quarter of the pages.
fn database_lines(store: &mut Store) -> ExitCode {
    let footprint = match store.footprint() {
        Ok(footprint) => footprint,
        Err(problem) => {
            println!("stats failed: catalog: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let human = crate::paths::human_bytes;
    println!(
        "stats: database {} (WAL {}), {} pages of {} B",
        human(footprint.file_bytes),
        human(footprint.wal_bytes),
        footprint.pages,
        footprint.page_size,
    );
    vacuum_guidance(&footprint);
    if let Some(sizes) = store.dbstat_sizes() {
        let top: Vec<String> = sizes
            .into_iter()
            .take(5)
            .map(|(name, bytes)| format!("{name} {}", human(bytes)))
            .collect();
        if !top.is_empty() {
            println!("stats: biggest objects {}", top.join(", "));
        }
    }
    ExitCode::SUCCESS
}

/// The `VACUUM` guidance: said when the freelist is more than a
/// quarter of the pages — the space a vacuum would give back. The verb
/// reports, it never repairs.
fn vacuum_guidance(footprint: &DbFootprint) {
    if !earns_vacuum(footprint) {
        return;
    }
    let share = footprint.freelist_pages * 100 / footprint.pages;
    println!(
        "stats: {} of {} pages ({share}%) are free — `VACUUM` would shrink the file",
        footprint.freelist_pages, footprint.pages,
    );
}

/// Whether the database's freelist is large enough to be worth a
/// vacuum.
fn earns_vacuum(footprint: &DbFootprint) -> bool {
    if footprint.pages == 0 {
        return false;
    }
    footprint.freelist_pages * 100 / footprint.pages > 25
}

/// The score spread: minimum, average, maximum, and how many rows carry
/// no score at all.
fn health_line(overview: &CatalogOverview) -> String {
    let bound = |value: Option<u64>| {
        value
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    };
    let average = overview
        .health_average
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "none".to_string());
    format!(
        "min {}, average {average}, max {} ({} unscored)",
        bound(overview.health_min),
        bound(overview.health_max),
        overview.health_unscored,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter-freelist database earns the vacuum line; a healthy
    /// one does not.
    #[test]
    fn the_vacuum_guidance_follows_the_freelist_share() {
        let healthy = DbFootprint {
            file_bytes: 1_000,
            wal_bytes: 0,
            pages: 100,
            freelist_pages: 20,
            page_size: 4096,
        };
        assert!(!earns_vacuum(&healthy), "20 of 100 free pages is fine");
        let bloated = DbFootprint {
            freelist_pages: 30,
            ..healthy
        };
        assert!(
            earns_vacuum(&bloated),
            "30 of 100 free pages earns the line"
        );
    }

    #[test]
    fn the_health_line_formats_the_spread() {
        let overview = CatalogOverview {
            repos_total: 3,
            repos_present: 3,
            repos_missing: 0,
            forks: 0,
            archived: 0,
            health_min: Some(2),
            health_average: Some(7.5),
            health_max: Some(11),
            health_unscored: 0,
            stars_total: 0,
            tagged_repos: 0,
        };
        let line = health_line(&overview);
        assert!(line.contains("min 2"));
        assert!(line.contains("average 7.5"));
        assert!(line.contains("max 11"));
        assert!(line.contains("0 unscored"));
    }
}
