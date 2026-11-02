//! The `export` and `import` verbs: the library out, and back in.
//!
//! Two formats, full stop. `json` is the catalog in the export shape —
//! the stars exporter's row plus the yagna extension (the user's tags
//! and categories, the stored health, the enrichment facts) — and
//! `sql` is a consistent copy of the database file, taken through
//! SQLite's backup API so it is never a torn copy. The import merges
//! a JSON export into the existing library: repositories are matched by
//! their identity, the user's tags are merged and never clobbered, and
//! importing the same file twice changes nothing the second time.
//!
//! Items: `run_export` — the export verb; `run_import` — the import
//! verb; `ExportFormat` — the two formats; `default_path` — the file
//! an export writes when no path is given.
//! Depends on: yagna_store (the export read, the merge, the backup),
//! yagna_core::export (the shape), chrono (the export's stamp).
//! Called by: crate::main (the `export` and `import` verbs).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::Utc;
use yagna_core::export::ExportFile;

/// The two export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ExportFormat {
    /// The catalog as JSON, in the export shape.
    #[default]
    Json,
    /// A consistent copy of the database file.
    Sql,
}

impl ExportFormat {
    /// The format's name on the command line.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
        }
    }

    /// The format's file extension.
    fn extension(self) -> &'static str {
        match self {
            ExportFormat::Json => "json",
            ExportFormat::Sql => "db",
        }
    }
}

/// The export verb: writes the catalog in the chosen format. An
/// existing destination is refused — an export is a new file, never a
/// quiet overwrite of something the user already has — and so is the
/// live database file itself.
pub(crate) fn run_export(format: ExportFormat, out: Option<PathBuf>) -> ExitCode {
    let store = match crate::paths::open_catalog() {
        Ok(store) => store,
        Err(problem) => {
            println!("export failed: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let path = out.unwrap_or_else(|| default_path(format));
    if path.exists() {
        println!(
            "export failed: {} already exists — move it or name another file",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    if is_the_live_database(&path) {
        println!(
            "export failed: {} is the live database — an export never overwrites it",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    match format {
        ExportFormat::Json => write_json(&store, &path),
        ExportFormat::Sql => write_sql(&store, &path),
    }
}

/// Whether the path names the catalog this library lives in.
fn is_the_live_database(path: &Path) -> bool {
    let Ok(live) = crate::paths::catalog() else {
        return false;
    };
    std::fs::canonicalize(path).ok() == std::fs::canonicalize(&live).ok()
}

/// The import verb: merges a JSON export into the library.
pub(crate) fn run_import(path: &Path) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(problem) => {
            println!("import failed: {} — {problem}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let file: ExportFile = match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(problem) => {
            println!(
                "import failed: {} is not an export — {problem}",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    if file.format_version != yagna_core::export::EXPORT_FORMAT_VERSION {
        println!(
            "import failed: the file's export shape is version {}, this build reads {} — upgrade yAGna (or the file's maker)",
            file.format_version,
            yagna_core::export::EXPORT_FORMAT_VERSION
        );
        return ExitCode::FAILURE;
    }
    let mut store = match crate::paths::open_catalog() {
        Ok(store) => store,
        Err(problem) => {
            println!("import failed: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let backup = pre_import_backup(&store);
    match store.import_repos(&file.repos, Utc::now()) {
        Ok(report) => {
            println!(
                "import: {} inserted, {} updated, {} unchanged, {} skipped, {} tag(s) attached, from {}",
                report.inserted,
                report.updated,
                report.unchanged,
                report.skipped,
                report.tagged,
                file.repos.len(),
            );
            if let Some(backup) = &backup {
                println!(
                    "import: the library as it was is kept at {}",
                    backup.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(problem) => {
            println!("import failed: catalog: {problem}");
            if let Some(backup) = &backup {
                println!(
                    "import: the library is untouched; the pre-import copy is at {}",
                    backup.display()
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// A consistent copy of the library beside it, stamped with the import
/// time — the undo the merge cannot give back. `None` (said out loud
/// by no one: the import still runs) when the copy cannot be taken.
fn pre_import_backup(store: &yagna_store::Store) -> Option<PathBuf> {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let name = format!("yagna-pre-import-{stamp}.db");
    let Ok(dir) = crate::paths::data_dir() else {
        return None;
    };
    let path = dir.join(name);
    store.backup_to(&path).ok()?;
    Some(path)
}

/// The JSON export: the catalog in the export shape, written as one
/// readable file.
fn write_json(store: &yagna_store::Store, path: &Path) -> ExitCode {
    let format = ExportFormat::Json;
    let repos = match store.export_repos() {
        Ok(repos) => repos,
        Err(problem) => {
            println!("export failed: catalog: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let file = ExportFile::new(repos, Utc::now());
    let text = match serde_json::to_string_pretty(&file) {
        Ok(text) => text,
        Err(problem) => {
            println!("export failed: the file could not be written — {problem}");
            return ExitCode::FAILURE;
        }
    };
    match std::fs::write(path, text) {
        Ok(()) => {
            println!(
                "export: {} repos written to {} ({})",
                file.repos.len(),
                path.display(),
                format.as_str(),
            );
            ExitCode::SUCCESS
        }
        Err(problem) => {
            println!("export failed: {} — {problem}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// The SQL export: a consistent copy of the database file.
fn write_sql(store: &yagna_store::Store, path: &Path) -> ExitCode {
    let format = ExportFormat::Sql;
    match store.backup_to(path) {
        Ok(()) => {
            println!(
                "export: the database copied to {} ({})",
                path.display(),
                format.as_str(),
            );
            ExitCode::SUCCESS
        }
        Err(problem) => {
            println!("export failed: {} — {problem}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// The file an export writes when the user names no path: today's date
/// and the format's extension, in the working directory.
fn default_path(format: ExportFormat) -> PathBuf {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    PathBuf::from(format!("yagna-export-{stamp}.{}", format.extension()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_formats_have_their_names_and_extensions() {
        assert_eq!(ExportFormat::Json.as_str(), "json");
        assert_eq!(ExportFormat::Sql.as_str(), "sql");
        assert_eq!(ExportFormat::default(), ExportFormat::Json);
    }

    #[test]
    fn the_default_export_file_is_named_after_the_day() {
        let path = default_path(ExportFormat::Json);
        let name = path.to_string_lossy().to_string();
        assert!(name.starts_with("yagna-export-"), "{name}");
        assert!(name.ends_with(".json"), "{name}");
        let sql = default_path(ExportFormat::Sql);
        assert!(sql.to_string_lossy().ends_with(".db"));
    }
}
