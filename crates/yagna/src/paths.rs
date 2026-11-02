//! The data locations the CLI verbs share: where the catalog lives and
//! how it is opened.
//!
//! Items: `project_dirs` — the per-platform data location; `db_path` —
//! the catalog's file name; `cache_dir`/`cache` — the asset cache's
//! directory; `data_dir` — the data directory, created;
//! `catalog` — the path, directory created; `open_catalog` —
//! the path opened through the store facade; `human_bytes` — the size
//! format the cache's lines speak.
//! Depends on: directories (the per-platform data location), yagna_store
//! (the disk door).
//! Called by: the verbs that touch the disk — sync, enrich, stats,
//! doctor, transfer, prefetch, settings — and crate::ui.

use std::path::PathBuf;

use directories::ProjectDirs;

/// The per-platform data location for yAGna.
pub(crate) fn project_dirs() -> Result<ProjectDirs, String> {
    ProjectDirs::from("", "", "yagna")
        .ok_or_else(|| "this platform has no data locations".to_string())
}

/// The catalog database's file name, inside whatever data directory.
const DB_FILE: &str = "yagna.db";

/// The catalog database's file inside a data directory.
pub(crate) fn db_path(dirs: &ProjectDirs) -> PathBuf {
    dirs.data_dir().join(DB_FILE)
}

/// The asset cache's directory: the images the READMEs point at, one
/// file per asset id. The doctor probes the same directory.
pub(crate) fn cache_dir(dirs: &ProjectDirs) -> PathBuf {
    dirs.cache_dir().to_path_buf()
}

/// The asset cache's directory, created if absent.
pub(crate) fn cache() -> Result<PathBuf, String> {
    let dirs = project_dirs()?;
    let cache = cache_dir(&dirs);
    std::fs::create_dir_all(&cache).map_err(|error| {
        format!(
            "asset cache {}: cannot be created: {error}",
            cache.display()
        )
    })?;
    Ok(cache)
}

/// The data directory, created if absent — the digest's save lands here
/// when the user's own folders are not reachable.
pub(crate) fn data_dir() -> Result<PathBuf, String> {
    let dirs = project_dirs()?;
    std::fs::create_dir_all(dirs.data_dir()).map_err(|error| {
        format!(
            "data directory {}: cannot be created: {error}",
            dirs.data_dir().display()
        )
    })?;
    Ok(dirs.data_dir().to_path_buf())
}

/// The catalog's path, creating the data directory first — an absent
/// directory is not a failure, it is an empty catalog waiting.
pub(crate) fn catalog() -> Result<PathBuf, String> {
    let dir = data_dir()?;
    Ok(dir.join(DB_FILE))
}

/// Opens the catalog, mapping every failure to a user-facing line.
pub(crate) fn open_catalog() -> Result<yagna_store::Store, String> {
    let path = catalog()?;
    yagna_store::Store::open(&path)
        .map_err(|error| format!("database at {}: {error}", path.display()))
}

/// A size as the cache's lines speak it: whole bytes below a kilobyte,
/// one decimal above that, in the biggest unit that keeps the number
/// at or above one.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [("B", 1), ("KB", 1 << 10), ("MB", 1 << 20), ("GB", 1 << 30)];
    let mut chosen = UNITS[0];
    for unit in UNITS {
        if bytes >= unit.1 {
            chosen = unit;
        }
    }
    let value = bytes as f64 / chosen.1 as f64;
    if chosen.1 == 1 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", chosen.0)
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    /// The size format the cache's lines speak: whole bytes below a
    /// kilobyte, one decimal above, biggest unit that keeps one.
    #[test]
    fn sizes_speak_in_the_biggest_unit_that_keeps_one() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(16 * 1024 * 1024), "16.0 MB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }
}
