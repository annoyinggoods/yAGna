//! The settings the app runs with: one model struct, one TOML file
//! (`<data>/settings.toml`), defaults in code.
//!
//! Every setting lands here with its default in the same change — no
//! ad-hoc keys. The file is read at launch; a missing file is the
//! defaults and silence, a broken file is the defaults plus the loud
//! line the caller shows, never a crash. The model is typed (`SortKey`,
//! `ImageSetting`); the file's text representation is this module's
//! `SettingsFile`, and the conversion is where a bad value falls back.
//!
//! Items: `Settings` — the settings one session runs with (including
//! the PEOPLE tab's `followed_users`, the reader's `font_scale` and the
//! folder the local repositories are discovered in);
//! `ImageSetting` — when images are fetched (`preview` | `tab-open` |
//! `none`); `load` — the file read; `save` — the file written;
//! `DEFAULT_CACHE_CAP_BYTES` — the cap the file starts with.
//! Depends on: yagna_core (`SortKey`), crate::paths (the data
//! directory), serde and toml (the file format).
//! Called by: crate::ui (the launch path and the settings surface).

use yagna_core::model::SortKey;

/// The cache cap a fresh settings file starts with: 2 GB.
pub(crate) const DEFAULT_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The page size a fresh settings file starts with.
pub(crate) const DEFAULT_PAGE_SIZE: u32 = 500;

/// The largest page a settings file may ask for: the page query is one
/// SQLite statement, and an absurd page is a frozen frame, not a
/// preference.
pub(crate) const MAX_PAGE_SIZE: u32 = 2_000;

/// The theme a fresh settings file starts with.
pub(crate) const DEFAULT_THEME: &str = "dark";

/// The type scale a fresh settings file starts with: the ramp's own
/// size, in percent. The ramp clamps whatever the file names, so a
/// wild value is a floor, never a crash.
pub(crate) const DEFAULT_FONT_SCALE: u8 = 100;

/// When images are fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ImageSetting {
    /// The expanded row fetches its preview image; an open tab fetches
    /// the repository's images.
    #[default]
    Preview,
    /// The expanded row shows placeholders; only an open tab fetches.
    TabOpen,
    /// Nothing is fetched; what is cached is shown.
    None,
}

impl ImageSetting {
    /// The label the settings surface shows; also the file's spelling.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ImageSetting::Preview => "preview",
            ImageSetting::TabOpen => "tab-open",
            ImageSetting::None => "none",
        }
    }

    /// The label's inverse — the file's spelling back into the policy.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        [Self::Preview, Self::TabOpen, Self::None]
            .into_iter()
            .find(|policy| policy.label() == text)
    }

    /// The next policy in the cycle the settings surface walks.
    pub(crate) fn next(self) -> Self {
        match self {
            ImageSetting::Preview => ImageSetting::TabOpen,
            ImageSetting::TabOpen => ImageSetting::None,
            ImageSetting::None => ImageSetting::Preview,
        }
    }

    /// Whether an open tab may enqueue the repository's images.
    pub(crate) fn tab_may_fetch(self) -> bool {
        matches!(self, ImageSetting::Preview | ImageSetting::TabOpen)
    }

    /// Whether an expanded row may fetch its preview image.
    pub(crate) fn row_may_fetch(self) -> bool {
        matches!(self, ImageSetting::Preview)
    }
}

/// The settings one session runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The active theme's name — one of `theme::THEME_NAMES`, or a
    /// name the user's themes directory carries.
    pub theme: String,
    /// The sort the first page of a session opens under.
    pub sort_default: SortKey,
    /// Whether the launch path fires the starred sweep a few seconds
    /// after first frame. Off by default: a sweep is a choice.
    pub auto_update_on_launch: bool,
    /// When images are fetched.
    pub image_policy: ImageSetting,
    /// The asset cache's cap, in bytes.
    pub cache_cap_bytes: u64,
    /// Whether the tab asks the forge for the repository's exact first
    /// commit. On by default: the date costs one request, and the
    /// provenance is what makes it honest.
    pub exact_first_commit: bool,
    /// Whether the open rows survive a restart (the store carries the
    /// set). Off by default: a session starts clean.
    pub persist_expansion: bool,
    /// Rows per page.
    pub page_size: u32,
    /// The logins the PEOPLE tab follows: other users' starred lists
    /// as read-only shadows. Validated on use — a name that stops
    /// being usable is skipped, never fatal.
    pub followed_users: Vec<String>,
    /// The reader's type scale, in percent of the ramp's base sizes.
    /// The ramp clamps it; the file keeps what the reader chose.
    pub font_scale: u8,
    /// The folder the local repositories are discovered in — the
    /// reader's own folder of clones. `None` until the reader names
    /// one; the app never guesses a path.
    pub local_folder: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.to_string(),
            sort_default: SortKey::StarredAt,
            auto_update_on_launch: false,
            image_policy: ImageSetting::Preview,
            cache_cap_bytes: DEFAULT_CACHE_CAP_BYTES,
            exact_first_commit: true,
            persist_expansion: false,
            page_size: DEFAULT_PAGE_SIZE,
            followed_users: Vec::new(),
            font_scale: DEFAULT_FONT_SCALE,
            local_folder: None,
        }
    }
}

/// Loads the settings file: the defaults when it is absent, the
/// defaults plus a loud line when it cannot be read or parsed, the
/// file's values (each validated, each bad one falling back) when it
/// parses. The second element is that loud line, said in the status
/// slot.
pub(crate) fn load() -> (Settings, Option<String>) {
    let Ok(dir) = crate::paths::data_dir() else {
        return (Settings::default(), None);
    };
    load_from(&dir.join("settings.toml"))
}

/// Writes the settings file, creating the data directory if needed.
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    let dir = crate::paths::data_dir()?;
    save_to(&dir.join("settings.toml"), settings)
}

/// The file's text shape: the model's typed fields as strings, so a
/// broken value falls back instead of failing the whole file.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SettingsFile {
    theme: String,
    sort_default: String,
    auto_update_on_launch: bool,
    image_policy: String,
    cache_cap_bytes: u64,
    exact_first_commit: bool,
    persist_expansion: bool,
    page_size: u32,
    #[serde(default)]
    followed_users: Vec<String>,
    #[serde(default)]
    font_scale: u8,
    /// The folder the local repositories are discovered in — empty
    /// until the reader names one.
    #[serde(default)]
    local_folder: String,
}

impl SettingsFile {
    /// The file's values into the model: every field is validated, and
    /// a value that does not hold falls back to its default.
    fn into_settings(self) -> Settings {
        let defaults = Settings::default();
        let sort_default = SortKey::ALL
            .into_iter()
            .find(|key| key.label() == self.sort_default)
            .unwrap_or(defaults.sort_default);
        let page_size = match self.page_size {
            0 => defaults.page_size,
            size if size > MAX_PAGE_SIZE => defaults.page_size,
            size => size,
        };
        let cache_cap_bytes = if self.cache_cap_bytes == 0 {
            defaults.cache_cap_bytes
        } else {
            self.cache_cap_bytes
        };
        let theme = if self.theme.is_empty() {
            defaults.theme
        } else {
            self.theme
        };
        let font_scale = if self.font_scale == 0 {
            defaults.font_scale
        } else {
            self.font_scale
        };
        let image_policy = ImageSetting::parse(&self.image_policy).unwrap_or(defaults.image_policy);
        let local_folder = self.local_folder.trim().to_string();
        let local_folder = (!local_folder.is_empty()).then_some(local_folder);
        let mut followed_users: Vec<String> = self
            .followed_users
            .iter()
            .map(|login| login.trim().to_string())
            .filter(|login| !login.is_empty())
            .collect();
        followed_users.sort_by_key(|login| login.to_lowercase());
        followed_users.dedup_by_key(|login| login.to_lowercase());
        followed_users.truncate(100);
        Settings {
            theme,
            sort_default,
            auto_update_on_launch: self.auto_update_on_launch,
            image_policy,
            cache_cap_bytes,
            exact_first_commit: self.exact_first_commit,
            persist_expansion: self.persist_expansion,
            page_size,
            followed_users,
            font_scale,
            local_folder,
        }
    }

    /// The model into the file's text shape.
    fn from_settings(settings: &Settings) -> Self {
        Self {
            theme: settings.theme.clone(),
            sort_default: settings.sort_default.label().to_string(),
            auto_update_on_launch: settings.auto_update_on_launch,
            image_policy: settings.image_policy.label().to_string(),
            cache_cap_bytes: settings.cache_cap_bytes,
            exact_first_commit: settings.exact_first_commit,
            persist_expansion: settings.persist_expansion,
            page_size: settings.page_size,
            followed_users: settings.followed_users.clone(),
            font_scale: settings.font_scale,
            local_folder: settings.local_folder.clone().unwrap_or_default(),
        }
    }
}

/// `load`, against a caller-chosen path — the test seam. The production
/// `load` is this with the real data directory.
fn load_from(path: &std::path::Path) -> (Settings, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<SettingsFile>(&text) {
            Ok(file) => (file.into_settings(), None),
            Err(error) => (
                Settings::default(),
                Some(format!(
                    "settings file {}: {error} — running on the defaults",
                    path.display()
                )),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
        Err(error) => (
            Settings::default(),
            Some(format!(
                "settings file {}: {error} — running on the defaults",
                path.display()
            )),
        ),
    }
}

/// `save`, against a caller-chosen path — the test seam. The write is
/// a temp file and a rename: a crash mid-write leaves the previous
/// settings standing, never a half file.
fn save_to(path: &std::path::Path, settings: &Settings) -> Result<(), String> {
    let text = toml::to_string_pretty(&SettingsFile::from_settings(settings))
        .map_err(|error| format!("settings cannot be written: {error}"))?;
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text)
        .map_err(|error| format!("settings file {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|error| format!("settings file {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's defaults, pinned: the sweep is a choice, the cache
    /// holds two gigabytes, the session starts clean.
    #[test]
    fn the_exact_first_commit_is_on_by_default() {
        let defaults = Settings::default();
        assert!(
            defaults.exact_first_commit,
            "the provenance is worth one request"
        );
        assert!(!defaults.auto_update_on_launch);
        assert_eq!(defaults.image_policy, ImageSetting::Preview);
        assert_eq!(defaults.cache_cap_bytes, DEFAULT_CACHE_CAP_BYTES);
        assert!(!defaults.persist_expansion);
        assert_eq!(defaults.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(defaults.theme, DEFAULT_THEME);
        assert_eq!(defaults.sort_default, SortKey::StarredAt);
        assert_eq!(
            defaults.font_scale, DEFAULT_FONT_SCALE,
            "the type ramp starts at its own size"
        );
    }

    /// A missing settings file is the defaults and silence.
    #[test]
    fn an_absent_settings_file_is_the_defaults() {
        let scratch =
            std::env::temp_dir().join(format!("yagna-settings-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let path = scratch.join("settings.toml");
        let loaded = load_from(&path);
        assert_eq!(loaded.0, Settings::default());
        assert_eq!(loaded.1, None);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A broken settings file is the defaults plus a loud line, never a
    /// crash.
    #[test]
    fn a_broken_settings_file_is_the_defaults_and_a_line() {
        let scratch =
            std::env::temp_dir().join(format!("yagna-settings-broken-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch");
        let path = scratch.join("settings.toml");
        std::fs::write(&path, "page_size = = = broken").expect("write");
        let loaded = load_from(&path);
        assert_eq!(loaded.0, Settings::default());
        let line = loaded.1.expect("loud");
        assert!(line.contains("settings.toml"), "the line names the file");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A written file reads back to the settings it was written from.
    #[test]
    fn a_saved_file_reads_back() {
        let scratch =
            std::env::temp_dir().join(format!("yagna-settings-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch");
        let path = scratch.join("settings.toml");
        let settings = Settings {
            theme: "light".to_string(),
            page_size: 100,
            image_policy: ImageSetting::TabOpen,
            ..Settings::default()
        };
        save_to(&path, &settings).expect("written");
        let loaded = load_from(&path);
        assert_eq!(loaded.0, settings);
        assert_eq!(loaded.1, None);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Bad values in an otherwise-parseable file fall back per field.
    #[test]
    fn bad_values_fall_back_per_field() {
        let text =
            "sort_default = \"nonsense\"\npage_size = 0\ncache_cap_bytes = 0\ntheme = \"\"\n";
        let file: SettingsFile = toml::from_str(text).expect("the shape parses");
        let settings = file.into_settings();
        assert_eq!(settings.sort_default, SortKey::StarredAt);
        assert_eq!(settings.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(settings.cache_cap_bytes, DEFAULT_CACHE_CAP_BYTES);
        assert_eq!(settings.theme, DEFAULT_THEME);
    }

    /// The followed list survives a round trip, trimmed and deduped.
    #[test]
    fn followed_users_round_trip_trimmed_and_deduped() {
        let text = "followed_users = [\" octocat \", \"\", \"octocat\", \"friend\"]\n";
        let file: SettingsFile = toml::from_str(text).expect("the shape parses");
        let settings = file.into_settings();
        assert_eq!(settings.followed_users, ["friend", "octocat"]);
        let back = SettingsFile::from_settings(&settings);
        assert_eq!(back.followed_users, settings.followed_users);
    }

    /// The local folder survives the file, and an empty one means no
    /// folder at all: the app never guesses a path.
    #[test]
    fn the_local_folder_round_trips_and_an_empty_one_is_none() {
        let text = "local_folder = \"C:/repos\"\n";
        let file: SettingsFile = toml::from_str(text).expect("the shape parses");
        assert_eq!(
            file.into_settings().local_folder.as_deref(),
            Some("C:/repos")
        );
        let empty: SettingsFile = toml::from_str("").expect("the shape parses");
        assert_eq!(empty.into_settings().local_folder, None);
        let blank: SettingsFile =
            toml::from_str("local_folder = \"  \"").expect("the shape parses");
        assert_eq!(blank.into_settings().local_folder, None);
    }

    /// The image policies' gates, as the enqueue paths call them.
    #[test]
    fn image_settings_gate_the_two_fetch_paths() {
        assert!(ImageSetting::Preview.row_may_fetch());
        assert!(ImageSetting::Preview.tab_may_fetch());
        assert!(!ImageSetting::TabOpen.row_may_fetch());
        assert!(ImageSetting::TabOpen.tab_may_fetch());
        assert!(!ImageSetting::None.row_may_fetch());
        assert!(!ImageSetting::None.tab_may_fetch());
    }

    /// The reader's type scale survives the file, and a file that names
    /// none falls back to the ramp's own size.
    #[test]
    fn the_font_scale_round_trips_and_falls_back() {
        let scaled = Settings {
            font_scale: 120,
            ..Settings::default()
        };
        let back = SettingsFile::from_settings(&scaled);
        assert_eq!(back.font_scale, 120);
        assert_eq!(back.into_settings().font_scale, 120);

        let text = "theme = \"dark\"\n";
        let file: SettingsFile = toml::from_str(text).expect("the shape parses");
        assert_eq!(
            file.into_settings().font_scale,
            DEFAULT_FONT_SCALE,
            "a file that names no scale runs on the ramp's own size"
        );
    }
}
