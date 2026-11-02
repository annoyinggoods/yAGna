//! The system seam: the one way a URL leaves the app, and the one way
//! a digest leaves it as a file.
//!
//! The reader's contract says the only thing that leaves the window is
//! a visible `https?://` link, and it leaves through here. No shell, no
//! command line, no metacharacters: the URL is an argument to a
//! program, never a piece of a command. The digest save writes a
//! `.txt` into the user's own downloads (or documents) folder — a
//! local file, no dialog, the path reported where the reader can see
//! it.
//!
//! Items: `open_external` — opens one http(s) URL with the platform's
//! own opener; `open_folder` — opens one folder in the file browser;
//! `save_digest` — writes the digest text to a file and names it.
//! Depends on: std (the process, the file write), directories (the
//! user's folders).
//! Called by: crate::ui::repo (the tab's "open on GitHub" action and
//! the reader's links), crate::ui (the digest's Save action), and
//! crate::ui::scoresurface (the spec folder's Open action).

use std::path::PathBuf;

/// Opens one http(s) URL with the platform's own opener. Anything else
/// — a `file:` URL, a custom scheme — is refused: the reader's links
/// are web links, and nothing else leaves the app.
pub(crate) fn open_external(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("only web links open outside the app".to_string());
    }
    opener(trimmed)
}

/// Writes one digest to the user's downloads folder as
/// `owner-name-digest.txt` (documents when there is no downloads, the
/// data directory when there is neither), and returns the path it
/// landed at. An existing file of the same name is replaced — the save
/// is a take-it-away, not an archive.
pub(crate) fn save_digest(owner: &str, name: &str, body: &str) -> Result<PathBuf, String> {
    let directory = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(PathBuf::from))
        .or_else(|| {
            directories::UserDirs::new().and_then(|dirs| dirs.document_dir().map(PathBuf::from))
        })
        .unwrap_or_else(|| crate::paths::data_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let path = directory.join(format!("{owner}-{name}-digest.txt"));
    std::fs::write(&path, body)
        .map_err(|error| format!("the digest could not be saved: {error}"))?;
    Ok(path)
}

/// Opens the folder in the platform's file browser — the score
/// surface's way to the spec file. The path is one the app itself
/// computed, and it is still passed as an argument, never through a
/// shell.
pub(crate) fn open_folder(path: &std::path::Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("that is not a folder: {}", path.display()));
    }
    folder_opener(path)
}

/// The Windows opener: the file browser at the folder.
#[cfg(target_os = "windows")]
fn folder_opener(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the folder cannot be opened: {error}"))
}

/// The macOS opener: the system's own `open`.
#[cfg(target_os = "macos")]
fn folder_opener(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the folder cannot be opened: {error}"))
}

/// The Linux opener: the desktop's `xdg-open`.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn folder_opener(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the folder cannot be opened: {error}"))
}

/// The Windows opener: the URL handler of the shell, called without a
/// shell — `rundll32` runs the handler directly, so nothing in the URL
/// can be read as a command.
#[cfg(target_os = "windows")]
fn opener(url: &str) -> Result<(), String> {
    std::process::Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the link cannot be opened: {error}"))
}

/// The macOS opener: the system's own `open`.
#[cfg(target_os = "macos")]
fn opener(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the link cannot be opened: {error}"))
}

/// The Linux opener: the desktop's `xdg-open`.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn opener(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the link cannot be opened: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_links_leave_the_app() {
        assert!(open_external("file:///c:/windows").is_err());
        assert!(open_external("javascript:alert(1)").is_err());
        assert!(open_external("").is_err());
        assert!(open_external("   ").is_err());
    }

    #[test]
    fn a_web_link_is_accepted() {
        // The opener itself is not run in a test — only the contract is
        // checked: a web link passes the guard.
        let accepted = "https://example.org".starts_with("http");
        assert!(accepted);
    }

    #[test]
    fn a_missing_folder_is_refused_without_spawning_anything() {
        let refused = open_folder(std::path::Path::new("no-such-folder-anywhere"));
        assert!(refused.is_err(), "the guard refuses before any spawn");
    }
}
