//! The curated icon set: the shapes the shell draws, named once.
//!
//! Items: `Icon` — the icons the app draws; `Icon::asset` — the path
//! the built-in asset source serves it under; `Icon::filled_asset` — the
//! filled variant, for the active and primary states; `Icon::name` — the
//! accessible name a screen reader and a tooltip read; `ICON_SIZE` — the
//! box an icon is drawn in.
//! Core functions: `fn all` — every icon, in the order they are declared.
//! Depends on: gpui (the pixel size constant).
//! Called by: crate::ui (the shell, the buttons, the menu).
//! Calls: nothing but gpui.

/// The box an icon is drawn in: a square that sits beside the label it
/// belongs to.
pub(crate) const ICON_SIZE: gpui::Pixels = gpui::px(16.0);

/// The curated icons: the shapes the shell draws. Each one is named
/// once, so the path it is served under, the filled shape it has and
/// the name it is announced with live in a single table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Icon {
    Star,
    Refresh,
    Search,
    Settings,
    User,
    Users,
    Compass,
    Book,
    Tag,
    Close,
    ChevronDown,
    ArrowUpRight,
    AlertTriangle,
    Check,
    Filter,
    Folder,
}

/// What an icon is: the outline it is served under, the filled shape it
/// has, and the name it is announced with.
struct Spec {
    /// The path the asset source serves the outline under.
    asset: &'static str,
    /// The path the asset source serves the filled shape under; `None`
    /// when the icon has no filled shape of its own.
    filled: Option<&'static str>,
    /// The name a screen reader reads and a tooltip shows.
    name: &'static str,
}

impl Icon {
    /// The icon's own facts. The one place an icon's three are listed,
    /// so a new icon is one table row and one entry in `all`.
    fn spec(self) -> Spec {
        match self {
            Icon::Star => Spec {
                asset: "yagna-icon/star.svg",
                filled: Some("yagna-icon/star-filled.svg"),
                name: "star",
            },
            Icon::Refresh => Spec {
                asset: "yagna-icon/refresh.svg",
                filled: None,
                name: "refresh",
            },
            Icon::Search => Spec {
                asset: "yagna-icon/search.svg",
                filled: None,
                name: "search",
            },
            Icon::Settings => Spec {
                asset: "yagna-icon/settings.svg",
                filled: Some("yagna-icon/settings-filled.svg"),
                name: "settings",
            },
            Icon::User => Spec {
                asset: "yagna-icon/user.svg",
                filled: None,
                name: "user",
            },
            Icon::Users => Spec {
                asset: "yagna-icon/users.svg",
                filled: None,
                name: "users",
            },
            Icon::Compass => Spec {
                asset: "yagna-icon/compass.svg",
                filled: None,
                name: "discover",
            },
            Icon::Book => Spec {
                asset: "yagna-icon/book.svg",
                filled: Some("yagna-icon/book-filled.svg"),
                name: "library",
            },
            Icon::Tag => Spec {
                asset: "yagna-icon/tag.svg",
                filled: None,
                name: "tag",
            },
            Icon::Close => Spec {
                asset: "yagna-icon/x.svg",
                filled: None,
                name: "close",
            },
            Icon::ChevronDown => Spec {
                asset: "yagna-icon/chevron-down.svg",
                filled: None,
                name: "expand",
            },
            Icon::ArrowUpRight => Spec {
                asset: "yagna-icon/arrow-up-right.svg",
                filled: None,
                name: "open in a new tab",
            },
            Icon::AlertTriangle => Spec {
                asset: "yagna-icon/alert-triangle.svg",
                filled: None,
                name: "warning",
            },
            Icon::Check => Spec {
                asset: "yagna-icon/check.svg",
                filled: None,
                name: "done",
            },
            Icon::Filter => Spec {
                asset: "yagna-icon/filter.svg",
                filled: None,
                name: "filter",
            },
            Icon::Folder => Spec {
                asset: "yagna-icon/folder.svg",
                filled: None,
                name: "folder",
            },
        }
    }

    /// The path the asset source serves this icon's outline under.
    pub(crate) fn asset(self) -> &'static str {
        self.spec().asset
    }

    /// The path of this icon's filled shape, for the active and the
    /// primary state; `None` when the icon has no filled shape.
    pub(crate) fn filled_asset(self) -> Option<&'static str> {
        self.spec().filled
    }

    /// The name a screen reader reads and a tooltip shows.
    pub(crate) fn name(self) -> &'static str {
        self.spec().name
    }
}

/// Every icon, in the order they are declared.
pub(crate) fn all() -> [Icon; 16] {
    [
        Icon::Star,
        Icon::Refresh,
        Icon::Search,
        Icon::Settings,
        Icon::User,
        Icon::Users,
        Icon::Compass,
        Icon::Book,
        Icon::Tag,
        Icon::Close,
        Icon::ChevronDown,
        Icon::ArrowUpRight,
        Icon::AlertTriangle,
        Icon::Check,
        Icon::Filter,
        Icon::Folder,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// The directory the icon documents ship in.
    fn icon_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("ui")
            .join("assets")
            .join("yagna-icon")
    }

    /// The file name inside the icon directory that a served path names.
    fn file_name(asset: &str) -> &str {
        asset.strip_prefix("yagna-icon/").unwrap_or(asset)
    }

    #[test]
    fn every_icon_serves_its_own_path() {
        let paths: HashSet<&str> = all().iter().map(|icon| icon.asset()).collect();
        assert_eq!(
            paths.len(),
            all().len(),
            "each icon is served under a path of its own"
        );
    }

    #[test]
    fn every_filled_shape_serves_its_own_path() {
        let filled: Vec<&str> = all()
            .iter()
            .filter_map(|icon| icon.filled_asset())
            .collect();
        let distinct: HashSet<&str> = filled.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            filled.len(),
            "each filled shape is served under a path of its own"
        );
        assert_eq!(filled.len(), 3, "three icons carry a filled shape");
    }

    #[test]
    fn every_icon_is_named() {
        for icon in all() {
            assert!(!icon.name().is_empty(), "{icon:?} is named");
        }
    }

    /// The set names every variant the enum has, and the enum has no
    /// other: `Icon::spec` matches every variant without a wildcard arm,
    /// so a variant missing from that table could not compile.
    #[test]
    fn the_set_names_every_icon_the_enum_has() {
        let icons = all();
        let distinct: HashSet<Icon> = icons.iter().copied().collect();
        assert_eq!(distinct.len(), icons.len(), "each icon appears once");
        assert_eq!(icons.len(), 16, "the set names sixteen icons");
    }

    /// Every path the set names is a document that ships with the crate:
    /// the metadata and the copied documents agree.
    #[test]
    fn every_named_path_is_an_svg_that_ships() {
        let paths: Vec<PathBuf> = all()
            .iter()
            .flat_map(|icon| {
                [Some(icon.asset()), icon.filled_asset()]
                    .into_iter()
                    .flatten()
            })
            .map(|asset| icon_directory().join(file_name(asset)))
            .collect();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} reads: {error}", path.display()));
            assert!(text.contains("<svg"), "{} is an SVG", path.display());
        }
    }
}
