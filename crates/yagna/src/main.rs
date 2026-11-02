//! The yAGna binary: the CLI verbs and the app.
//!
//! Items: `Cli` — the argument parser; `Command` — the verbs (`doctor`,
//! `sync` (with `--enrich`), `stats`, `export`/`import`, `prefetch`,
//! `mcp`, `--mark-startup`, and no verb — the window); the verb modules —
//! doctor, sync, enrich, digestgen, mcp, pacing, prefetch, stats, startup
//! (the cold-start marks), transfer (export/import), ui (the window);
//! token and paths — the edges those verbs share.
//! Depends on: yagna_core (shared types), yagna_store (the disk door),
//! yagna_forge (the network door), clap (arguments), directories (data
//! locations), gpui (the pinned UI framework, carried for the surfaces).
//! Called by: the user's shell.
//! Calls: the store and forge facades, through the verb modules and the
//! ui module.

mod digestgen;
mod doctor;
mod enrich;
mod fitspec;
mod mcp;
mod pacing;
mod paths;
mod prefetch;
mod settings;
mod startup;
mod stats;
mod sync;
mod token;
mod transfer;
mod ui;
mod updater;

use clap::{Parser, Subcommand};

/// The yAGna command line.
#[derive(Parser)]
#[command(name = "yagna", version, about = "A local-first GitHub stars catalog")]
struct Cli {
    /// Measure the launch path and print the timings, then exit: the
    /// cold-start gate's instrumented run.
    #[arg(long, global = true)]
    mark_startup: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Probe the environment: token, API, data directory, database, cache.
    Doctor,
    /// Pull the starred sweep into the local catalog.
    Sync {
        /// Work the enrichment queue (READMEs, license texts, trees)
        /// after the sweep.
        #[arg(long)]
        enrich: bool,
        /// Print one line per sweep page, and enrich progress detail.
        #[arg(long)]
        verbose: bool,
    },
    /// Count what the catalog holds, and what it costs on disk.
    Stats,
    /// Write the catalog out: the export shape as JSON, or a copy of
    /// the database.
    Export {
        /// The format: the export shape as JSON, or the database file.
        #[arg(long, default_value = "json")]
        format: String,
        /// Where the file goes — the working directory by default.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Merge an exported catalog back into this library.
    Import {
        /// The export file to read.
        path: std::path::PathBuf,
    },
    /// Pull a scope's repositories into the cache, before the reader
    /// needs them.
    Prefetch {
        /// The scope: the repositories whose primary language is this.
        #[arg(long, group = "scope")]
        language: Option<String>,
        /// The scope: the repositories carrying every one of these tags.
        #[arg(long, group = "scope")]
        tag: Vec<String>,
        /// The scope: one repository, as owner/name.
        #[arg(long, group = "scope")]
        repository: Option<String>,
        /// The images: none, the first 2 MB of each repository's
        /// images, or all of them.
        #[arg(long, default_value = "none")]
        images: String,
        /// The licence texts.
        #[arg(long)]
        licenses: bool,
        /// The folder structures.
        #[arg(long)]
        trees: bool,
    },
    /// Serve the catalog over MCP (stdio). For coding agents.
    Mcp,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Doctor) => doctor::run(),
        Some(Command::Sync { enrich, verbose }) => sync::run(verbose, enrich),
        Some(Command::Stats) => stats::run(),
        Some(Command::Export { format, out }) => {
            let format = match format.as_str() {
                "json" => transfer::ExportFormat::Json,
                "sql" => transfer::ExportFormat::Sql,
                other => {
                    println!("export failed: unknown format {other:?} — json or sql");
                    return std::process::ExitCode::FAILURE;
                }
            };
            transfer::run_export(format, out)
        }
        Some(Command::Import { path }) => transfer::run_import(&path),
        Some(Command::Prefetch {
            language,
            tag,
            repository,
            images,
            licenses,
            trees,
        }) => prefetch::run(prefetch::Flags {
            language,
            tag,
            repository,
            images,
            licenses,
            trees,
        }),
        Some(Command::Mcp) => mcp::server::run(),
        None => ui::run(cli.mark_startup),
    }
}
