//! The local repositories: the clones on this machine, found by pointing
//! the app at a folder, and read without ever being written to.
//!
//! A local entry is a fact about the disk, never about the service: it
//! stands beside the catalog's row of the same repository, joined on the
//! origin's owner and name, and it stands alone when the catalog has no
//! row for it. The scan reads `.git` directories to find the worktrees,
//! reads their configs for the origin, their `HEAD` for the branch and
//! their reflog for the last commit's date — plain files, no `git`
//! binary, no network.
//!
//! Nothing here writes to the reader's working copy: the folder is
//! opened for reading, and the only bytes this module ever writes are
//! the store's own rows.
//!
//! Items: `LocalScan` — one worktree the scan found; `LocalPreview` — a
//! file's text, as far as the cap allows; `LocalError` — the failures
//! the disk can report.
//! Core functions: `scan_folder` — the worktrees under a folder;
//! `read_directory` — one folder's entries; `read_preview` — one file's
//! text.
//! Depends on: std::fs (the disk), chrono (the stamps), yagna_core
//! (the entries it reports).
//! Called by: the app (the scan, the local page).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use yagna_core::model::{LocalEntry, LocalEntryKind};

use crate::{Store, StoreError};

/// How deep the scan looks for worktrees: the chosen folder's own
/// subfolders, four levels down. A deeper folder is the reader's own
/// business, not a repository's.
pub const SCAN_DEPTH: usize = 4;

/// The largest file the preview reads: a megabyte is a long read, and a
/// longer one is a file the reader does not want in a text box.
pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// How many files one worktree's count walks before it stops counting.
const FILE_COUNT_CAP: usize = 250_000;

/// The failures the disk can report.
#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("the folder cannot be read: {0}")]
    Io(String),
    #[error("that path is not a folder: {0}")]
    NotAFolder(String),
    #[error("that path leaves the worktree: {0}")]
    OutsideWorktree(String),
}

/// One worktree the scan found: where it is, and what its own files
/// say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalScan {
    /// The worktree's absolute path.
    pub path: String,
    /// The directory's own name.
    pub name: String,
    /// The owner the origin names — `None` when the origin is absent or
    /// names a forge the catalog does not know.
    pub owner: Option<String>,
    /// The repository name the origin names.
    pub repo_name: Option<String>,
    /// The origin URL, as the worktree's own config carries it.
    pub origin: Option<String>,
    /// The branch the worktree is on — `None` when it is detached.
    pub branch: Option<String>,
    /// The last commit's date, as the worktree's own reflog records it.
    pub last_commit_at: Option<DateTime<Utc>>,
    /// How many files the worktree holds, git's own objects aside.
    pub file_count: u64,
    /// How many bytes those files hold.
    pub total_bytes: u64,
}

/// One file's text, as far as the cap allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPreview {
    /// The file's text — empty when the file is binary.
    pub text: String,
    /// Whether the file is longer than the cap.
    pub truncated: bool,
    /// Whether the file is binary: it carries bytes that are not text.
    pub binary: bool,
    /// The file's size on disk, in bytes.
    pub bytes: u64,
}

/// Scans a folder for worktrees: every directory under it that carries
/// a `.git`, up to the depth cap. Symlinks are never followed, and a
/// worktree is never descended into — the scan finds repositories, not
/// their contents.
pub fn scan_folder(root: &Path) -> Result<Vec<LocalScan>, LocalError> {
    let meta = fs::metadata(root).map_err(|error| LocalError::Io(error.to_string()))?;
    if !meta.is_dir() {
        return Err(LocalError::NotAFolder(root.display().to_string()));
    }
    // The chosen folder may itself be a worktree. Then it is the one
    // repository the scan finds — a worktree is never descended into.
    if git_dir(root).is_some() {
        return Ok(vec![scan_repository(root)]);
    }
    let mut found: Vec<LocalScan> = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = pending.pop() {
        let (repositories, folders) = classify(&dir);
        found.extend(repositories);
        if depth >= SCAN_DEPTH {
            continue;
        }
        pending.extend(folders.into_iter().map(|path| (path, depth + 1)));
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(found)
}

/// Reads one folder of a worktree: its entries, by name. The path each
/// entry carries is relative to the worktree's root, so the page can ask
/// for a child by that path.
pub fn read_directory(root: &Path, relative: &str) -> Result<Vec<LocalEntry>, LocalError> {
    let directory = resolve(root, relative)?;
    let (folders, files) = split(&directory);
    let mut entries: Vec<LocalEntry> = Vec::new();
    entries.extend(
        folders
            .into_iter()
            .map(|path| folder_entry(&path, relative)),
    );
    entries.extend(
        files
            .into_iter()
            .map(|(path, size)| file_entry(&path, size, relative)),
    );
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// Reads one file of a worktree: its text up to the cap. A binary file
/// comes back with no text and the binary flag set — the page shows a
/// placeholder with its size, never a wall of bytes.
pub fn read_preview(root: &Path, relative: &str) -> Result<LocalPreview, LocalError> {
    let path = resolve(root, relative)?;
    let meta = fs::metadata(&path).map_err(|error| LocalError::Io(error.to_string()))?;
    let bytes = meta.len();
    let cap = PREVIEW_MAX_BYTES.min(bytes) as usize;
    let mut buffer = vec![0u8; cap];
    let mut file = fs::File::open(&path).map_err(|error| LocalError::Io(error.to_string()))?;
    let read = file
        .read(&mut buffer)
        .map_err(|error| LocalError::Io(error.to_string()))?;
    buffer.truncate(read);
    let binary = buffer.contains(&0);
    let text = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(&buffer).to_string()
    };
    Ok(LocalPreview {
        text,
        truncated: bytes > PREVIEW_MAX_BYTES,
        binary,
        bytes,
    })
}

/// The worktree's own git directory: the `.git` folder, or the folder
/// the `.git` file points at (a linked worktree).
fn git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot = worktree.join(".git");
    let meta = fs::symlink_metadata(&dot).ok()?;
    if meta.is_dir() {
        return Some(dot);
    }
    let text = fs::read_to_string(&dot).ok()?;
    let pointer = text.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(pointer);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    })
}

/// The worktree's origin URL, as its own config carries it.
fn read_origin(git: &Path) -> Option<String> {
    let config = fs::read_to_string(git.join("config")).ok()?;
    origin_url(&config)
}

/// The origin URL a git config names.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = is_origin_section(line);
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        return Some(value.trim().to_string());
    }
    None
}

/// Whether a git config section is the origin's own.
fn is_origin_section(line: &str) -> bool {
    let compact = line.replace(' ', "");
    compact.eq_ignore_ascii_case("[remote\"origin\"]")
}

/// The owner and the repository name an origin URL names — only for
/// GitHub, the one forge the catalog knows. Any other origin, or one
/// this cannot read, leaves the entry standalone: it is still an entry.
fn parse_origin(url: &str) -> Option<(String, String)> {
    let text = strip_known_prefixes(url.trim());
    let (host, path) = split_host_and_path(text)?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?.to_string();
    let name = parts.next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner, name.to_string()))
}

/// The url without the schemes and the user prefix a git remote may
/// carry.
fn strip_known_prefixes(text: &str) -> &str {
    let text = text.strip_prefix("ssh://").unwrap_or(text);
    let text = text.strip_prefix("https://").unwrap_or(text);
    let text = text.strip_prefix("http://").unwrap_or(text);
    let text = text.strip_prefix("git://").unwrap_or(text);
    text.strip_prefix("git@").unwrap_or(text)
}

/// The host and the path of a remote url — the two shapes a remote
/// takes: `host/owner/name` and `host:owner/name`.
fn split_host_and_path(text: &str) -> Option<(&str, &str)> {
    if let Some((host, rest)) = text.split_once(':') {
        if !rest.contains('/') {
            return None;
        }
        return Some((host, rest));
    }
    text.split_once('/')
}

/// The branch the worktree is on — `None` when its `HEAD` names no
/// branch (a detached head).
fn read_branch(git: &Path) -> Option<String> {
    let head = fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let reference = head.strip_prefix("ref:")?.trim();
    let branch = reference.strip_prefix("refs/heads/")?;
    if branch.is_empty() {
        return None;
    }
    Some(branch.to_string())
}

/// The last commit's date, as the worktree's own reflog records it. The
/// reflog's last line ends with the timestamp and the zone; the name
/// before them may carry spaces, so the stamp is read from the end.
fn read_last_commit(git: &Path) -> Option<DateTime<Utc>> {
    let log = fs::read_to_string(git.join("logs").join("HEAD")).ok()?;
    let last = log.lines().rfind(|line| !line.trim().is_empty())?;
    // The line's head carries the stamp; the tail (after the tab) is
    // the reader's own message about the move.
    let head = last.split('\t').next().unwrap_or(last);
    let mut tokens = head.split_whitespace().rev();
    let zone = tokens.next()?;
    let stamp = tokens.next()?;
    if zone.is_empty() {
        return None;
    }
    let seconds: i64 = stamp.parse().ok()?;
    Utc.timestamp_opt(seconds, 0).single()
}

/// One worktree, read from its own files.
fn scan_repository(path: &Path) -> LocalScan {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let git = git_dir(path);
    let origin = git.as_deref().and_then(read_origin);
    let (owner, repo_name) = origin
        .as_deref()
        .and_then(parse_origin)
        .map(|(owner, name)| (Some(owner), Some(name)))
        .unwrap_or((None, None));
    let branch = git.as_deref().and_then(read_branch);
    let last_commit_at = git.as_deref().and_then(read_last_commit);
    let (file_count, total_bytes) = measure(path);
    LocalScan {
        path: path.display().to_string(),
        name,
        owner,
        repo_name,
        origin,
        branch,
        last_commit_at,
        file_count,
        total_bytes,
    }
}

/// The directory's own worktrees and the subfolders worth descending
/// into — one walk, two answers.
fn classify(directory: &Path) -> (Vec<LocalScan>, Vec<PathBuf>) {
    let mut repositories: Vec<LocalScan> = Vec::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    for path in plain_directories(directory) {
        if git_dir(&path).is_some() {
            repositories.push(scan_repository(&path));
            continue;
        }
        folders.push(path);
    }
    (repositories, folders)
}

/// The directory's own subfolders, symlinks left out.
fn plain_directories(directory: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for entry in read_dir(directory) {
        let path = entry.path();
        if !is_plain_directory(&entry) {
            continue;
        }
        folders.push(path);
    }
    folders
}

/// Whether an entry is a directory that is not a symlink.
fn is_plain_directory(entry: &fs::DirEntry) -> bool {
    entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
}

/// What one directory entry is: a folder to walk, a file to count, or
/// neither.
enum Child {
    /// A folder to walk into.
    Folder(PathBuf),
    /// A file, with its size.
    File(PathBuf, u64),
}

/// The directory's children: the folders to walk, and the files to
/// count. Git's own directory is left out — a worktree's files are the
/// reader's, not git's objects.
fn split(directory: &Path) -> (Vec<PathBuf>, Vec<(PathBuf, u64)>) {
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    for entry in read_dir(directory) {
        match classify_entry(&entry) {
            Some(Child::Folder(path)) => folders.push(path),
            Some(Child::File(path, size)) => files.push((path, size)),
            None => continue,
        }
    }
    (folders, files)
}

/// One directory entry, read as a folder, as a file, or as nothing.
fn classify_entry(entry: &fs::DirEntry) -> Option<Child> {
    let path = entry.path();
    let kind = entry.file_type().ok()?;
    if kind.is_symlink() {
        return None;
    }
    if is_git_dir(&path) {
        return None;
    }
    if kind.is_dir() {
        return Some(Child::Folder(path));
    }
    let size = entry.metadata().ok()?.len();
    Some(Child::File(path, size))
}

/// Whether a path is the git directory itself.
fn is_git_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == ".git")
}

/// The directory's entries, or none when it cannot be read.
fn read_dir(directory: &Path) -> Vec<fs::DirEntry> {
    match fs::read_dir(directory) {
        Ok(entries) => entries.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

/// The worktree's own size: how many files it holds, and how many bytes
/// those files are. The walk stops at the cap — a huge worktree is a
/// number, not a crawl.
fn measure(root: &Path) -> (u64, u64) {
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let (folders, files) = split(&directory);
        count += files.len() as u64;
        bytes += files.iter().map(|(_, size)| *size).sum::<u64>();
        pending.extend(folders);
        if count as usize >= FILE_COUNT_CAP {
            break;
        }
    }
    (count, bytes)
}

/// The folder's entry, with its relative path.
fn folder_entry(path: &Path, parent: &str) -> LocalEntry {
    LocalEntry {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: relative_path(parent, path),
        kind: LocalEntryKind::Directory,
        size: None,
    }
}

/// The file's entry, with its relative path and its size.
fn file_entry(path: &Path, size: u64, parent: &str) -> LocalEntry {
    LocalEntry {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: relative_path(parent, path),
        kind: LocalEntryKind::File,
        size: Some(size),
    }
}

/// The path of an entry inside the worktree, relative to its root.
fn relative_path(parent: &str, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if parent.is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    }
}

/// The path a relative name resolves to inside the worktree. A name
/// that climbs out of the worktree is refused: the page reads the
/// reader's own files, and never one outside them.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, LocalError> {
    if relative.split('/').any(|part| part == "..") {
        return Err(LocalError::OutsideWorktree(relative.to_string()));
    }
    let path = root.join(relative);
    Ok(path)
}

impl Store {
    /// Writes one scan: the worktrees it found, the ones that went away
    /// marked missing, and the catalog's rows matched to the origins.
    ///
    /// The write is one transaction, and it is never a delete: a folder
    /// that went missing keeps its row, because the reader may move it
    /// back.
    pub fn replace_local_scan(
        &mut self,
        root: &str,
        scans: &[LocalScan],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let stamp = crate::catalog::stamp(now);
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "UPDATE local_repos SET missing_at = ?1 WHERE missing_at IS NULL",
            [&stamp],
        )?;
        for scan in scans {
            transaction.execute(
                "INSERT INTO local_repos
                   (root, path, name, owner, repo_name, origin, branch, last_commit_at,
                    file_count, total_bytes, linked_rowid, missing_at, scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11)
                 ON CONFLICT(path) DO UPDATE SET
                   root = excluded.root,
                   name = excluded.name,
                   owner = excluded.owner,
                   repo_name = excluded.repo_name,
                   origin = excluded.origin,
                   branch = excluded.branch,
                   last_commit_at = excluded.last_commit_at,
                   file_count = excluded.file_count,
                   total_bytes = excluded.total_bytes,
                   missing_at = NULL,
                   scanned_at = excluded.scanned_at",
                rusqlite::params![
                    root,
                    scan.path,
                    scan.name,
                    scan.owner,
                    scan.repo_name,
                    scan.origin,
                    scan.branch,
                    scan.last_commit_at.map(crate::catalog::stamp),
                    scan.file_count as i64,
                    scan.total_bytes as i64,
                    stamp,
                ],
            )?;
        }
        link_local_repos(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Every local entry, the ones that are there first, and the missing
    /// ones last — a folder that went away is a fact, not a wall.
    pub fn local_repos(&self) -> Result<Vec<yagna_core::model::LocalRepo>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, root, path, name, owner, repo_name, origin, branch, last_commit_at,
                    file_count, total_bytes, linked_rowid, missing_at, scanned_at
               FROM local_repos
              ORDER BY (missing_at IS NOT NULL) ASC, path COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map([], map_local_repo)?;
        let mut repos = Vec::new();
        for row in rows {
            repos.push(row?);
        }
        Ok(repos)
    }

    /// One local entry.
    pub fn local_repo(&self, id: i64) -> Result<Option<yagna_core::model::LocalRepo>, StoreError> {
        let repo = self
            .conn
            .query_row(
                "SELECT id, root, path, name, owner, repo_name, origin, branch, last_commit_at,
                        file_count, total_bytes, linked_rowid, missing_at, scanned_at
                   FROM local_repos WHERE id = ?1",
                [id],
                map_local_repo,
            )
            .ok();
        Ok(repo)
    }
}

/// Matches the local entries to the catalog's rows: an entry whose
/// origin names a repository the catalog holds is linked to it, and an
/// entry with no such origin stands alone.
fn link_local_repos(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE local_repos SET linked_rowid = (
           SELECT r.id FROM repos r
            WHERE r.forge = 'github'
              AND r.owner = local_repos.owner
              AND r.name = local_repos.repo_name
         )
         WHERE owner IS NOT NULL AND repo_name IS NOT NULL",
        [],
    )?;
    transaction.execute(
        "UPDATE local_repos SET linked_rowid = NULL
          WHERE owner IS NULL OR repo_name IS NULL",
        [],
    )?;
    Ok(())
}

/// Maps one local row into its record.
fn map_local_repo(row: &rusqlite::Row<'_>) -> rusqlite::Result<yagna_core::model::LocalRepo> {
    Ok(yagna_core::model::LocalRepo {
        id: row.get(0)?,
        root: row.get(1)?,
        path: row.get(2)?,
        name: row.get(3)?,
        owner: row.get(4)?,
        repo_name: row.get(5)?,
        origin: row.get(6)?,
        branch: row.get(7)?,
        last_commit_at: row.get::<_, Option<String>>(8)?.map(|at| parse_stamp(&at)),
        file_count: row.get::<_, i64>(9)?.max(0) as u64,
        total_bytes: row.get::<_, i64>(10)?.max(0) as u64,
        linked_rowid: row.get(11)?,
        missing_at: row.get::<_, Option<String>>(12)?.map(|at| parse_stamp(&at)),
        scanned_at: parse_stamp(&row.get::<_, String>(13)?),
    })
}

/// A stored stamp, read back.
fn parse_stamp(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc.timestamp_opt(0, 0).single().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worktree in a temp directory: a `.git` folder with a config, a
    /// `HEAD` and a reflog, and a couple of files beside it.
    fn worktree(root: &Path, name: &str, origin: Option<&str>) -> PathBuf {
        let path = root.join(name);
        let git = path.join(".git");
        fs::create_dir_all(&git).expect("git directory");
        fs::create_dir_all(path.join("src")).expect("source directory");
        fs::write(path.join("README.md"), "the readme\n").expect("readme");
        fs::write(path.join("src").join("main.rs"), "fn main() {}\n").expect("source");
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::create_dir_all(git.join("logs")).expect("logs directory");
        let config = match origin {
            Some(url) => format!(
                "[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
            ),
            None => "[core]\n\tbare = false\n".to_string(),
        };
        fs::write(git.join("config"), config).expect("config");
        fs::write(
            git.join("logs").join("HEAD"),
            "0000000000000000000000000000000000000000 1234567890abcdef1234567890abcdef12345678 Someone <someone@example.com> 1699999999 +0200\tcommit: the first commit\n",
        )
        .expect("reflog");
        path
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yagna-local-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn the_scan_finds_the_worktrees_under_a_folder() {
        let root = scratch("scan");
        worktree(&root, "first", Some("https://github.com/someone/first.git"));
        worktree(&root.join("nested"), "second", None);

        let found = scan_folder(&root).expect("scan");
        let names: Vec<&str> = found.iter().map(|scan| scan.name.as_str()).collect();
        assert_eq!(names, ["first", "second"], "both worktrees are found");
        let first = &found[0];
        assert_eq!(first.owner.as_deref(), Some("someone"));
        assert_eq!(first.repo_name.as_deref(), Some("first"));
        assert_eq!(first.branch.as_deref(), Some("main"));
        assert_eq!(first.file_count, 2, "the worktree's own files are counted");
        assert!(first.total_bytes > 0);
        assert!(first.last_commit_at.is_some(), "the reflog's date is read");
        let _ = fs::remove_dir_all(&root);
    }

    /// The folder the reader names may itself be a worktree: then it is
    /// the one repository the scan finds.
    #[test]
    fn a_folder_that_is_itself_a_worktree_is_found() {
        let root = scratch("root-worktree");
        worktree(&root, "repo", Some("https://github.com/someone/repo.git"));
        let found = scan_folder(&root).expect("scan");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "repo");
        assert_eq!(found[0].owner.as_deref(), Some("someone"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_worktree_without_an_origin_stays_standalone() {
        let root = scratch("standalone");
        worktree(&root, "plain", None);
        let found = scan_folder(&root).expect("scan");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].owner, None);
        assert_eq!(found[0].repo_name, None);
        assert_eq!(found[0].branch.as_deref(), Some("main"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_origins_are_read_in_every_shape() {
        assert_eq!(
            parse_origin("https://github.com/owner/name.git"),
            Some(("owner".to_string(), "name".to_string()))
        );
        assert_eq!(
            parse_origin("git@github.com:owner/name.git"),
            Some(("owner".to_string(), "name".to_string()))
        );
        assert_eq!(
            parse_origin("ssh://git@github.com/owner/name"),
            Some(("owner".to_string(), "name".to_string()))
        );
        assert_eq!(
            parse_origin("https://gitlab.com/owner/name.git"),
            None,
            "another forge's origin never links to the catalog"
        );
    }

    #[test]
    fn a_folder_is_browsed_and_a_file_is_read() {
        let root = scratch("browse");
        let path = worktree(&root, "repo", None);
        let entries = read_directory(&path, "").expect("read");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["README.md", "src"]);
        let readme = &entries[0];
        assert_eq!(readme.kind, LocalEntryKind::File);
        assert_eq!(readme.path, "README.md");

        let preview = read_preview(&path, "README.md").expect("preview");
        assert_eq!(preview.text, "the readme\n");
        assert!(!preview.binary);
        assert!(!preview.truncated);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_worktree_is_refused() {
        let root = scratch("escape");
        let path = worktree(&root, "repo", None);
        let error = read_directory(&path, "../..").expect_err("refused");
        assert!(matches!(error, LocalError::OutsideWorktree(_)));
        let error = read_preview(&path, "../../secrets").expect_err("refused");
        assert!(matches!(error, LocalError::OutsideWorktree(_)));
    }

    /// The store the scan writes into.
    fn store(label: &str) -> (crate::testing::Scratch, Store) {
        let scratch = crate::testing::Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    #[test]
    fn a_scan_is_written_and_linked_to_the_catalog() {
        let root = scratch("scan-write");
        worktree(&root, "first", Some("https://github.com/someone/first.git"));
        let found = scan_folder(&root).expect("scan");
        let (_scratch, mut store) = store("scan-write");
        let snapshot = crate::testing::sample_snapshot("someone", "first");
        store.upsert_snapshots(&[snapshot], now()).expect("seed");
        store
            .replace_local_scan(&root.display().to_string(), &found, now())
            .expect("write");

        let entries = store.local_repos().expect("entries");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "first");
        assert_eq!(entry.owner.as_deref(), Some("someone"));
        assert_eq!(entry.repo_name.as_deref(), Some("first"));
        assert!(entry.missing_at.is_none());
        assert!(
            entry.linked_rowid.is_some(),
            "the origin matched the catalog's row"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_entry_without_a_catalog_row_stands_alone() {
        let root = scratch("scan-alone");
        worktree(&root, "other", Some("https://github.com/nobody/other.git"));
        let found = scan_folder(&root).expect("scan");
        let (_scratch, mut store) = store("scan-alone");
        store
            .replace_local_scan(&root.display().to_string(), &found, now())
            .expect("write");

        let entries = store.local_repos().expect("entries");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].linked_rowid.is_none(), "no catalog row, no link");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_goes_away_is_marked_missing_and_never_deleted() {
        let root = scratch("scan-missing");
        worktree(&root, "first", None);
        let found = scan_folder(&root).expect("scan");
        let (_scratch, mut store) = store("scan-missing");
        let root_text = root.display().to_string();
        store
            .replace_local_scan(&root_text, &found, now())
            .expect("write");
        assert_eq!(store.local_repos().expect("entries").len(), 1);

        store
            .replace_local_scan(&root_text, &[], now())
            .expect("rescan");
        let entries = store.local_repos().expect("entries");
        assert_eq!(entries.len(), 1, "the row is never deleted");
        assert!(
            entries[0].missing_at.is_some(),
            "the folder that went away is marked"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_local_entry_carries_a_note_of_its_own() {
        let root = scratch("scan-note");
        worktree(&root, "first", None);
        let found = scan_folder(&root).expect("scan");
        let (_scratch, mut store) = store("scan-note");
        store
            .replace_local_scan(&root.display().to_string(), &found, now())
            .expect("write");
        let entries = store.local_repos().expect("entries");
        let subject = yagna_core::model::NoteSubject::Local(entries[0].id);
        store
            .set_note(subject, "the worktree I am working in", now())
            .expect("write");
        assert_eq!(
            store.note_of(subject).expect("read"),
            Some("the worktree I am working in".to_string())
        );
        let notes = store.notes_with_names().expect("list");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subject, subject);
        assert_eq!(notes[0].title(), "first");
        let _ = fs::remove_dir_all(&root);
    }
}
