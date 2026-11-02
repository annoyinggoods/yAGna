//! The assets the app may hand to the framework: the rasteriser's
//! limits, and the rule that keeps the process alive.
//!
//! The framework rasterises SVG files with resvg, and resvg aborts the
//! whole process on an SVG that carries a filter element — an
//! assertion in its filter pipeline, on a thread that cannot unwind.
//! A README's images come from other people's hosts, so the app checks
//! what it is about to show: a filtered SVG is never handed over, and
//! the image shows its quiet marker instead.
//!
//! The check is cheap: the head of the file decides whether it is an
//! SVG at all, and only an SVG is read whole.
//!
//! Items: `is_rasterisable` — the rule over bytes; `file_is_rasterisable`
//! — the rule over a cached file; `resolve_assets` — what each of a
//! repository's assets is, as the reader may see it.
//! Depends on: yagna_core::markdown (the rule), std (the file).
//! Called by: crate::ui (the image runner, the row preview, the tab's
//! assets).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use yagna_core::markdown::carries_svg_filter;
use yagna_core::mdrender::Resolved;
use yagna_store::AssetRecord;

/// How many bytes of a file are read to decide whether it is an SVG.
const HEAD_BYTES: usize = 512;

/// Whether the framework's rasteriser may be handed these bytes.
pub(crate) fn is_rasterisable(bytes: &[u8]) -> bool {
    !carries_svg_filter(bytes)
}

/// Whether the framework's rasteriser may be handed the cached file. A
/// file that cannot be read is not handed over either: the image shows
/// its quiet marker, and the app lives.
pub(crate) fn file_is_rasterisable(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; HEAD_BYTES];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    let head = &head[..read];
    let markup = head.windows(5).any(|window| window == b"<?xml")
        || head.windows(4).any(|window| window == b"<svg");
    if !markup {
        // A raster image: nothing in it can be a filter.
        return true;
    }
    match std::fs::read(path) {
        Ok(bytes) => is_rasterisable(&bytes),
        Err(_) => false,
    }
}

/// What each of a repository's assets is, as the reader may see it:
/// the cached file when the framework can rasterise it, still pending
/// when it is on its way, and not there when it is gone — or when the
/// framework would choke on it.
pub(crate) fn resolve_assets(assets: &[AssetRecord]) -> HashMap<String, Resolved> {
    assets
        .iter()
        .map(|asset| (asset.asset_id.clone(), resolve_one(asset)))
        .collect()
}

/// What one asset is. A badge is never a picture: the reader counts
/// the badges instead of looking at them, so a badge answers as a
/// badge however the cache holds it.
fn resolve_one(asset: &AssetRecord) -> Resolved {
    if asset.badge {
        return Resolved::Badge;
    }
    if asset.state == "failed" {
        return Resolved::Missing;
    }
    if asset.state != "present" {
        return Resolved::Pending;
    }
    let Some(path) = asset.path.as_ref() else {
        return Resolved::Pending;
    };
    if !file_is_rasterisable(path) {
        return Resolved::Missing;
    }
    Resolved::File(path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_filtered_svg_is_never_handed_over() {
        let bytes = br#"<svg xmlns="http://www.w3.org/2000/svg"><filter id="f"/></svg>"#;
        assert!(!is_rasterisable(bytes));
    }

    #[test]
    fn a_plain_svg_and_a_png_are_handed_over() {
        assert!(is_rasterisable(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" height="1"/></svg>"#
        ));
        assert!(is_rasterisable(b"\x89PNG\r\n\x1a\n not an svg"));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_handed_over() {
        let missing = Path::new("this-file-does-not-exist.img");
        assert!(!file_is_rasterisable(missing));
    }

    /// The states the reader sees, and the rule that keeps a filtered
    /// SVG out of the rasteriser.
    #[test]
    fn the_assets_resolve_into_the_three_states() {
        let cached = std::env::temp_dir().join("yagna-assets-resolve-test.img");
        std::fs::write(&cached, b"not an svg at all").expect("write the file");
        let assets = vec![
            record("cached", "present", cached.to_str()),
            record("queued", "missing", None),
            record("gone", "failed", None),
            record("lost", "present", Some("/nowhere/at/all.img")),
        ];
        let resolved = resolve_assets(&assets);
        assert_eq!(resolved.get("queued"), Some(&Resolved::Pending));
        assert_eq!(resolved.get("gone"), Some(&Resolved::Missing));
        assert_eq!(
            resolved.get("cached"),
            Some(&Resolved::File(cached.clone())),
            "a cached file the rasteriser can take is shown"
        );
        assert_eq!(
            resolved.get("lost"),
            Some(&Resolved::Missing),
            "a file that cannot be read is missing, not shown"
        );
        let _ = std::fs::remove_file(&cached);
    }

    /// The rule that keeps the process alive: an SVG with a filter is
    /// never handed to the rasteriser.
    #[test]
    fn a_filtered_svg_on_disk_is_never_handed_over() {
        let path = std::env::temp_dir().join("yagna-assets-filter-test.svg");
        std::fs::write(
            &path,
            br#"<svg xmlns="http://www.w3.org/2000/svg"><filter id="f"/></svg>"#,
        )
        .expect("write the file");
        assert!(!file_is_rasterisable(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// One asset record, as the store holds it.
    fn record(id: &str, state: &str, path: Option<&str>) -> AssetRecord {
        AssetRecord {
            asset_id: id.to_string(),
            url: format!("https://example.org/{id}.png"),
            badge: false,
            state: state.to_string(),
            path: path.map(PathBuf::from),
            bytes: None,
        }
    }
}
