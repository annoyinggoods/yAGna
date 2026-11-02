//! The theme applier: the one module allowed to name a color value.
//!
//! A theme is one TOML file of color roles (roles, never widgets): the
//! four defaults ship embedded in `themes/`, a user can override any of
//! them with `<data>/themes/<name>.toml`. The active palette is parsed
//! once and swapped as data — switching themes is `apply` plus a
//! repaint, nothing else. Every surface reads its colors through this
//! module's zero-argument functions; a color literal anywhere else is a
//! build failure (the crate's gate test enforces it).
//!
//! Items: `Palette` — the file format's roles; `Colors` — the parsed
//! palette; `THEME_NAMES` — the four embedded themes; `parse_palette`,
//! `palette_by_name`, `load_active` — the file side; `apply` — the
//! swap; the role functions — what every surface calls.
//! Depends on: gpui (the color type), serde (the file format), toml
//! (the parser), std (the active slot).
//! Called by: crate::ui (every render path) and crate::settings (the
//! theme picker's names).

use std::path::Path;
use std::sync::{OnceLock, RwLock};

use gpui::Rgba;
use serde::Deserialize;

/// The four themes that ship embedded, in picker order.
pub(crate) const THEME_NAMES: [&str; 4] = ["light", "dark", "dark-gray", "light-gray"];

/// The theme file's roles: one hex string each. Roles, not widgets —
/// a theme carries no knowledge of what it paints.
///
/// The roles come in families, and the families never share a fill:
/// the window ground, the cards, the panels, the status line, the
/// notification card and the tab strip each own their own surfaces,
/// so one never reads as another.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Palette {
    pub background: String,
    pub surface: String,
    pub surface_alt: String,
    pub text: String,
    pub text_muted: String,
    pub accent: String,
    pub border: String,
    pub health_good: String,
    pub health_mid: String,
    pub health_low: String,
    pub language_bar: String,
    /// The veil behind a modal. A hex color; the role function adds the
    /// translucency, so a theme file without the role still parses.
    #[serde(default = "default_scrim")]
    pub scrim: String,
    /// The notification card's ground — the one slot that is neither a
    /// panel nor a status line.
    #[serde(default = "default_notice_bg")]
    pub notice_bg: String,
    /// The notification card's ink.
    #[serde(default = "default_notice_text")]
    pub notice_text: String,
    /// The status line's quiet ink: a fact, not an alarm.
    #[serde(default = "default_status_info")]
    pub status_info: String,
    /// The status line's alarm ink.
    #[serde(default = "default_status_error")]
    pub status_error: String,
    /// The selected tab's ground.
    #[serde(default = "default_tab_active")]
    pub tab_active: String,
    /// The unselected tab's ground.
    #[serde(default = "default_tab_inactive")]
    pub tab_inactive: String,
}

/// The scrim's color when a theme file names none.
fn default_scrim() -> String {
    "#000000".to_string()
}

/// The notification ground when a theme file names none.
fn default_notice_bg() -> String {
    "#1b2735".to_string()
}

/// The notification ink when a theme file names none.
fn default_notice_text() -> String {
    "#d6e6f7".to_string()
}

/// The quiet status ink when a theme file names none.
fn default_status_info() -> String {
    "#9fc4e0".to_string()
}

/// The alarm status ink when a theme file names none.
fn default_status_error() -> String {
    "#f0a0a6".to_string()
}

/// The selected tab's ground when a theme file names none.
fn default_tab_active() -> String {
    "#2f3542".to_string()
}

/// The unselected tab's ground when a theme file names none.
fn default_tab_inactive() -> String {
    "#1e2128".to_string()
}

/// The active palette, parsed once. The applier functions read it.
static ACTIVE: OnceLock<RwLock<Colors>> = OnceLock::new();

/// The slot behind the role functions, seeded with the embedded dark
/// theme so a read before `apply` still paints (tests; the launch path
/// always applies first).
fn slot() -> &'static RwLock<Colors> {
    ACTIVE.get_or_init(|| {
        RwLock::new(parse_colors(&embedded_dark()).unwrap_or_else(|_| Colors::all_transparent()))
    })
}

/// The embedded dark theme, or the transparent stand-in when it cannot
/// parse (a build bug the fixture tests catch).
fn embedded_dark() -> Palette {
    palette_by_name("dark").unwrap_or_else(Palette::transparent)
}

/// A parsed palette.
#[derive(Debug, Clone, Copy)]
struct Colors {
    background: Rgba,
    surface: Rgba,
    surface_alt: Rgba,
    text: Rgba,
    text_muted: Rgba,
    accent: Rgba,
    border: Rgba,
    health_good: Rgba,
    health_mid: Rgba,
    health_low: Rgba,
    language_bar: Rgba,
    scrim: Rgba,
    notice_bg: Rgba,
    notice_text: Rgba,
    status_info: Rgba,
    status_error: Rgba,
    tab_active: Rgba,
    tab_inactive: Rgba,
}

/// The parsed palette behind the role functions, or the embedded dark
/// theme when nothing has been applied yet (tests, and a window opened
/// before `apply` ran — which the launch path never does).
fn active() -> Colors {
    *slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Swaps the active palette: parses every role, and on any bad role
/// keeps the old palette and says why. The caller repaints.
pub(crate) fn apply(palette: &Palette) -> Result<(), String> {
    let colors = parse_colors(palette)?;
    *slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = colors;
    Ok(())
}

/// Parses a theme file's text. The file may come from the binary or
/// from the user's themes directory — one format for both.
pub(crate) fn parse_palette(text: &str) -> Result<Palette, String> {
    toml::from_str(text).map_err(|error| error.to_string())
}

/// The embedded default theme by name, or `None` for an unknown name.
pub(crate) fn palette_by_name(name: &str) -> Option<Palette> {
    let text = match name {
        "light" => include_str!("themes/light.toml"),
        "dark" => include_str!("themes/dark.toml"),
        "dark-gray" => include_str!("themes/dark-gray.toml"),
        "light-gray" => include_str!("themes/light-gray.toml"),
        _ => return None,
    };
    parse_palette(text).ok()
}

/// The active palette for a theme name: the embedded default, overlaid
/// by `<data>/themes/<name>.toml` when the user wrote one. A broken
/// override file is an error that names the file and the parse error —
/// the caller falls back and shows the line.
pub(crate) fn load_active(name: &str, data_dir: Option<&Path>) -> Result<Palette, String> {
    let mut palette = palette_by_name(name)
        .ok_or_else(|| format!("unknown theme '{name}' — try one of {THEME_NAMES:?}"))?;
    let Some(dir) = data_dir else {
        return Ok(palette);
    };
    let path = dir.join("themes").join(format!("{name}.toml"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(palette);
    };
    let override_palette =
        parse_palette(&text).map_err(|error| format!("theme file {}: {error}", path.display()))?;
    palette = override_palette;
    Ok(palette)
}

/// Parses every role of a palette; the first bad role names itself.
fn parse_colors(palette: &Palette) -> Result<Colors, String> {
    Ok(Colors {
        background: role("background", &palette.background)?,
        surface: role("surface", &palette.surface)?,
        surface_alt: role("surface_alt", &palette.surface_alt)?,
        text: role("text", &palette.text)?,
        text_muted: role("text_muted", &palette.text_muted)?,
        accent: role("accent", &palette.accent)?,
        border: role("border", &palette.border)?,
        health_good: role("health_good", &palette.health_good)?,
        health_mid: role("health_mid", &palette.health_mid)?,
        health_low: role("health_low", &palette.health_low)?,
        language_bar: role("language_bar", &palette.language_bar)?,
        scrim: role("scrim", &palette.scrim)?,
        notice_bg: role("notice_bg", &palette.notice_bg)?,
        notice_text: role("notice_text", &palette.notice_text)?,
        status_info: role("status_info", &palette.status_info)?,
        status_error: role("status_error", &palette.status_error)?,
        tab_active: role("tab_active", &palette.tab_active)?,
        tab_inactive: role("tab_inactive", &palette.tab_inactive)?,
    })
}

/// One role's hex string into a color.
fn role(name: &str, hex: &str) -> Result<Rgba, String> {
    parse_hex(hex).ok_or_else(|| format!("theme role {name}: '{hex}' is not a #rrggbb color"))
}

/// A `#rrggbb` (or bare `rrggbb`) string into a color.
fn parse_hex(hex: &str) -> Option<Rgba> {
    let digits = hex.trim().trim_start_matches('#');
    if digits.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some(gpui::rgba((value << 8) | 0xff))
}

/// The window behind everything.
pub(crate) fn background() -> Rgba {
    active().background
}

/// Bars, cards and buttons.
pub(crate) fn surface() -> Rgba {
    active().surface
}

/// The pressed/hover surface, the focused row, the tag chips' object
/// surface: one shade between surface and background.
pub(crate) fn surface_alt() -> Rgba {
    active().surface_alt
}

/// The default ink.
pub(crate) fn text() -> Rgba {
    active().text
}

/// Secondary ink: metadata, the excerpt, disabled controls.
pub(crate) fn text_muted() -> Rgba {
    active().text_muted
}

/// The interactive color: selected sort, the user's tags, focus.
pub(crate) fn accent() -> Rgba {
    active().accent
}

/// Hairlines between rows and around fields.
pub(crate) fn border() -> Rgba {
    active().border
}

/// Health 10–12.
pub(crate) fn health_good() -> Rgba {
    active().health_good
}

/// Health 7–9.
pub(crate) fn health_mid() -> Rgba {
    active().health_mid
}

/// Health 0–3, and the error ink the status line speaks with.
pub(crate) fn health_low() -> Rgba {
    active().health_low
}

/// The neutral bar color of the user tab, when the data carries no
/// color of its own.
pub(crate) fn language_bar() -> Rgba {
    active().language_bar
}

/// The veil behind a modal: the scrim's color at half opacity.
pub(crate) fn scrim() -> gpui::Hsla {
    gpui::Hsla::from(active().scrim).opacity(0.5)
}

/// The notification card's ground — its own slot, never a panel's.
pub(crate) fn notice_bg() -> Rgba {
    active().notice_bg
}

/// The notification card's ink.
pub(crate) fn notice_text() -> Rgba {
    active().notice_text
}

/// The status line's quiet ink: a fact, not an alarm.
pub(crate) fn status_info() -> Rgba {
    active().status_info
}

/// The status line's alarm ink.
pub(crate) fn status_error() -> Rgba {
    active().status_error
}

/// The selected tab's ground — the neutral pill the active tab wears.
pub(crate) fn tab_active() -> Rgba {
    active().tab_active
}

/// The unselected tab's ground.
pub(crate) fn tab_inactive() -> Rgba {
    active().tab_inactive
}

/// The default ink as a `#rrggbb` string: the one format an SVG
/// document's fill attribute speaks.
pub(crate) fn text_hex() -> String {
    format!("#{:06x}", u32::from(active().text) >> 8)
}

/// The window's font family: explicit on Windows where the default
/// resolution varies; the system UI font elsewhere.
#[cfg(target_os = "windows")]
pub(crate) const UI_FONT: &str = "Segoe UI";
#[cfg(not(target_os = "windows"))]
pub(crate) const UI_FONT: &str = ".SystemUIFont";

/// The monospace family the README preview is painted with.
#[cfg(target_os = "windows")]
pub(crate) const MONO_FONT: &str = "Consolas";
#[cfg(not(target_os = "windows"))]
pub(crate) const MONO_FONT: &str = "Menlo";

/// The size the monospace text is painted with: the code, the tree, the
/// license text.
pub(crate) const MONO_SIZE: gpui::Pixels = gpui::px(12.0);

/// A color the data carried — a language's own color, as the service
/// sent it. The hex text is parsed here, so no other module has to know
/// what a color looks like; a color that does not parse (or is missing)
/// falls back to the bar color, and a bar-less caller passes its own.
pub(crate) fn data_color(hex: Option<&str>) -> Rgba {
    data_color_or(hex, language_bar())
}

/// Like `data_color`, with the caller's fallback.
pub(crate) fn data_color_or(hex: Option<&str>, fallback: Rgba) -> Rgba {
    let Some(text) = hex else {
        return fallback;
    };
    parse_hex(text).unwrap_or(fallback)
}

impl Palette {
    /// A palette whose every role is the transparent placeholder — the
    /// stand-in when the embedded defaults cannot parse, a build bug
    /// the fixture tests catch.
    fn transparent() -> Self {
        Self {
            background: String::new(),
            surface: String::new(),
            surface_alt: String::new(),
            text: String::new(),
            text_muted: String::new(),
            accent: String::new(),
            border: String::new(),
            health_good: String::new(),
            health_mid: String::new(),
            health_low: String::new(),
            language_bar: String::new(),
            scrim: String::new(),
            notice_bg: String::new(),
            notice_text: String::new(),
            status_info: String::new(),
            status_error: String::new(),
            tab_active: String::new(),
            tab_inactive: String::new(),
        }
    }
}

impl Colors {
    /// The every-role-transparent stand-in.
    fn all_transparent() -> Self {
        Colors {
            background: Rgba::default(),
            surface: Rgba::default(),
            surface_alt: Rgba::default(),
            text: Rgba::default(),
            text_muted: Rgba::default(),
            accent: Rgba::default(),
            border: Rgba::default(),
            health_good: Rgba::default(),
            health_mid: Rgba::default(),
            health_low: Rgba::default(),
            language_bar: Rgba::default(),
            scrim: Rgba::default(),
            notice_bg: Rgba::default(),
            notice_text: Rgba::default(),
            status_info: Rgba::default(),
            status_error: Rgba::default(),
            tab_active: Rgba::default(),
            tab_inactive: Rgba::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four embedded theme files parse and carry every role.
    #[test]
    fn every_embedded_theme_parses() {
        for name in THEME_NAMES {
            let palette =
                palette_by_name(name).unwrap_or_else(|| panic!("theme {name} is embedded"));
            parse_colors(&palette).unwrap_or_else(|error| panic!("theme {name}: {error}"));
        }
    }

    /// The contrast invariant: every theme's text roles clear WCAG AA
    /// against the grounds they are painted on, and the accent — the
    /// one saturated color — clears the 3:1 floor for large text and
    /// interface ink. The ratios are computed, never eyeballed.
    ///
    /// The pairs are the slots the app actually paints: body and
    /// secondary ink on the window ground and on cards, the
    /// notification card's own pair, the status line's inks, the tab
    /// strip's pair, and the data inks (health tiers, language bars) on
    /// a card.
    #[test]
    fn invariant_every_theme_clears_wcag_aa() {
        /// The AA floor for body text.
        const AA_TEXT: f32 = 4.5;
        /// The AA floor for large text and interface ink.
        const AA_LARGE: f32 = 3.0;

        /// The pairs that carry body text, with the floor each must
        /// clear. The footer and the deep-search bar paint their ink on
        /// `surface_alt` — their pairs are covered like every other
        /// slot the app paints.
        const TEXT_PAIRS: [(&str, &str, f32); 15] = [
            ("text", "background", AA_TEXT),
            ("text", "surface", AA_TEXT),
            ("text_muted", "background", AA_TEXT),
            ("text_muted", "surface", AA_TEXT),
            ("text", "surface_alt", AA_TEXT),
            ("text_muted", "surface_alt", AA_TEXT),
            ("notice_text", "notice_bg", AA_TEXT),
            ("status_info", "background", AA_TEXT),
            ("status_error", "background", AA_TEXT),
            ("text", "tab_active", AA_TEXT),
            ("text_muted", "tab_inactive", AA_TEXT),
            ("health_good", "surface", AA_TEXT),
            ("health_mid", "surface", AA_TEXT),
            ("health_low", "surface", AA_TEXT),
            ("language_bar", "surface", AA_TEXT),
        ];
        /// The pairs that carry large text and interface ink.
        const LARGE_PAIRS: [(&str, &str, f32); 3] = [
            ("accent", "background", AA_LARGE),
            ("accent", "surface", AA_LARGE),
            ("accent", "tab_active", AA_LARGE),
        ];

        for name in THEME_NAMES {
            let palette =
                palette_by_name(name).unwrap_or_else(|| panic!("theme {name} is embedded"));
            for (ink, ground, floor) in TEXT_PAIRS {
                let ratio = contrast_of(&palette, ink, ground);
                assert!(
                    ratio >= floor,
                    "theme {name}: {ink} on {ground} is {ratio:.2}:1, under {floor}:1"
                );
            }
            for (ink, ground, floor) in LARGE_PAIRS {
                let ratio = contrast_of(&palette, ink, ground);
                assert!(
                    ratio >= floor,
                    "theme {name}: {ink} on {ground} is {ratio:.2}:1, under {floor}:1"
                );
            }
        }
    }

    /// The contrast ratio between two of a palette's roles, computed
    /// from the hex strings the theme file carries.
    fn contrast_of(palette: &Palette, ink: &str, ground: &str) -> f32 {
        let ink = hex_of(palette, ink);
        let ground = hex_of(palette, ground);
        yagna_core::contrast::contrast_ratio(ink, ground)
    }

    /// One role's hex string as the contrast math reads it.
    fn hex_of(palette: &Palette, role: &str) -> yagna_core::contrast::Rgb {
        let hex = match role {
            "background" => &palette.background,
            "surface" => &palette.surface,
            "surface_alt" => &palette.surface_alt,
            "text" => &palette.text,
            "text_muted" => &palette.text_muted,
            "accent" => &palette.accent,
            "border" => &palette.border,
            "health_good" => &palette.health_good,
            "health_mid" => &palette.health_mid,
            "health_low" => &palette.health_low,
            "language_bar" => &palette.language_bar,
            "scrim" => &palette.scrim,
            "notice_bg" => &palette.notice_bg,
            "notice_text" => &palette.notice_text,
            "status_info" => &palette.status_info,
            "status_error" => &palette.status_error,
            "tab_active" => &palette.tab_active,
            "tab_inactive" => &palette.tab_inactive,
            other => panic!("unknown theme role {other}"),
        };
        let digits = hex.trim().trim_start_matches('#');
        assert_eq!(digits.len(), 6, "role {role} is not a hex color");
        let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).expect("hex digits");
        (channel(0), channel(2), channel(4))
    }

    /// A user override replaces the embedded palette; a broken one is a
    /// named error, never a silent fall-through.
    #[test]
    fn an_override_file_replaces_the_embedded_palette() {
        let dir = std::env::temp_dir().join(format!("yagna-theme-{}", std::process::id()));
        let themes = dir.join("themes");
        std::fs::create_dir_all(&themes).expect("test themes directory");
        std::fs::write(themes.join("dark.toml"), "background = \"nope\"\n")
            .expect("write override");
        let error = load_active("dark", Some(&dir)).expect_err("the override is broken");
        assert!(error.contains("dark.toml"), "the error names the file");
        std::fs::write(
            themes.join("dark.toml"),
            "background = \"#010203\"\nsurface = \"#040506\"\nsurface_alt = \"#070809\"\ntext = \"#0a0b0c\"\ntext_muted = \"#0d0e0f\"\naccent = \"#101112\"\nborder = \"#131415\"\nhealth_good = \"#161718\"\nhealth_mid = \"#191a1b\"\nhealth_low = \"#1c1d1e\"\nlanguage_bar = \"#1f2021\"\n",
        )
        .expect("write override");
        let palette = load_active("dark", Some(&dir)).expect("the override parses");
        assert_eq!(palette.background, "#010203");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown theme name is a named error, not a silent default.
    #[test]
    fn an_unknown_theme_name_is_an_error() {
        let error = load_active("nope", None).expect_err("refused");
        assert!(error.contains("nope"));
    }

    /// The applier swaps palettes as data: apply, read, apply again.
    #[test]
    fn applying_a_palette_swaps_the_roles() {
        let mut palette = palette_by_name("dark").expect("the dark theme is embedded");
        palette.background = "#112233".to_string();
        apply(&palette).expect("the palette parses");
        assert_eq!(background(), parse_hex("#112233").expect("parses"));
        apply(&palette_by_name("light").expect("embedded")).expect("the palette parses");
        assert_ne!(background(), parse_hex("#112233").expect("parses"));
    }

    /// A palette with one bad role is refused whole — half-applied
    /// themes would paint a split window.
    #[test]
    fn a_palette_with_one_bad_role_is_refused() {
        let mut palette = palette_by_name("dark").expect("embedded");
        palette.accent = "blue".to_string();
        let error = apply(&palette).expect_err("refused");
        assert!(error.contains("accent"));
        // The old palette survives: the next read still parses as a
        // color the window can paint with.
        assert_ne!(background(), Rgba::default());
    }

    /// A color the data carried parses; a missing or malformed one
    /// falls back.
    #[test]
    fn data_colors_fall_back_when_they_do_not_parse() {
        let fallback = parse_hex("#112233").expect("parses");
        assert_eq!(
            data_color_or(Some("#dea584"), fallback),
            parse_hex("#dea584").expect("parses")
        );
        assert_eq!(data_color_or(None, fallback), fallback);
        assert_eq!(data_color_or(Some("zzz"), fallback), fallback);
    }
}
