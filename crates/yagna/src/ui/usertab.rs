//! The USER tab: the catalog's own numbers, as the pitch's stats
//! surface — totals, stars by language, the health tiers, the top tags
//! and topics, and the starred-per-year spread.
//!
//! Every number arrives as one store aggregate (the `stats_*` queries);
//! this module only renders them. Bars are text — the role colors carry
//! the tier and language inks, a language's own color when the data
//! carries one.
//!
//! Items: `UserStatsData` — what the store gave the tab; `render` — the
//! tab's element; `BAR_CHARS` — the bars' length.
//! Depends on: yagna_store (the aggregate records), crate::ui::theme
//! (the colors), gpui (the elements).
//! Called by: crate::ui (the list pane, when the user tab is in front).

use gpui::{SharedString, div, prelude::*};
use yagna_store::{CatalogOverview, HealthBuckets, LanguageStars, NameCount, YearCount};

use super::rows::RowLayout;
use super::theme;

/// The longest a text bar gets, in characters.
const BAR_CHARS: usize = 24;

/// What the store gave the user tab: the overview's two summary
/// numbers and the four aggregates, fetched fresh on every open.
#[derive(Debug, Clone)]
pub(crate) struct UserStatsData {
    pub overview: CatalogOverview,
    pub languages: Vec<LanguageStars>,
    pub health: HealthBuckets,
    pub years: Vec<YearCount>,
    pub tags: Vec<NameCount>,
    pub topics: Vec<NameCount>,
}

/// The user tab: a scrollable column of sections.
pub(crate) fn render(data: &UserStatsData) -> gpui::Stateful<gpui::Div> {
    div()
        .id("user-tab")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(super::repo::TabLayout::SCROLLBAR)
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .bg(theme::background())
        .text_color(theme::text())
        .child(render_summary(data))
        .child(render_languages(data))
        .child(render_health(data))
        .child(render_years(data))
        .child(render_names("the user's top tags", &data.tags))
        .child(render_names("the catalog's top topics", &data.topics))
}

/// The summary line: the four totals the tab opens with.
fn render_summary(data: &UserStatsData) -> gpui::Div {
    let overview = &data.overview;
    let line = format!(
        "{} repositories · {} stars · {} tagged · {} missing",
        overview.repos_present, overview.stars_total, overview.tagged_repos, overview.repos_missing,
    );
    section(
        "the library",
        div()
            .text_size(RowLayout::top_size())
            .text_color(theme::text())
            .child(SharedString::from(line)),
    )
}

/// Stars by language: one bar per language, in the language's own
/// color, heaviest first.
fn render_languages(data: &UserStatsData) -> gpui::Div {
    let heaviest = data
        .languages
        .first()
        .map(|language| language.stars)
        .unwrap_or(0);
    let rows = data
        .languages
        .iter()
        .map(|language| {
            let ink = theme::data_color(language.color.as_deref());
            let label = format!(
                "{} · {} ({})",
                language.name, language.stars, language.repos
            );
            bar_row(&label, language.stars, heaviest, ink)
        })
        .collect::<Vec<_>>();
    section("stars by language", rows_column(rows))
}

/// The health tiers: four bars in the tier inks.
fn render_health(data: &UserStatsData) -> gpui::Div {
    health_distribution(&data.health)
}

/// The health tiers: four bars in the tier inks. Shared with the score
/// surface — one distribution, one rendering.
pub(crate) fn health_distribution(buckets: &HealthBuckets) -> gpui::Div {
    let total = (buckets.strong + buckets.solid + buckets.weak + buckets.poor).max(1);
    let tiers = [
        ("scored 10–12", buckets.strong, theme::health_good()),
        ("scored 7–9", buckets.solid, theme::health_mid()),
        ("scored 4–6", buckets.weak, theme::text_muted()),
        ("scored 0–3", buckets.poor, theme::health_low()),
    ];
    let rows = tiers
        .into_iter()
        .map(|(label, count, ink)| {
            let label = format!("{label} · {count}");
            bar_row(&label, count, total, ink)
        })
        .collect::<Vec<_>>();
    section("health distribution", rows_column(rows))
}

/// The starred-per-year spread: one bar per year, oldest first.
fn render_years(data: &UserStatsData) -> gpui::Div {
    let heaviest = data.years.iter().map(|year| year.repos).max().unwrap_or(0);
    let rows = data
        .years
        .iter()
        .map(|year| {
            let label = format!("{} · {}", year.year, year.repos);
            bar_row(&label, year.repos, heaviest, theme::accent())
        })
        .collect::<Vec<_>>();
    let column = if rows.is_empty() {
        rows_column(vec![muted_line("nothing is starred yet")])
    } else {
        rows_column(rows)
    };
    section("starred per year", column)
}

/// A named-count section: the top tags, the top topics.
fn render_names(title: &str, counts: &[NameCount]) -> gpui::Div {
    let heaviest = counts.iter().map(|count| count.count).max().unwrap_or(0);
    let rows = counts
        .iter()
        .map(|count| {
            let label = format!("{} · {}", count.name, count.count);
            bar_row(&label, count.count, heaviest, theme::accent())
        })
        .collect::<Vec<_>>();
    let column = if rows.is_empty() {
        rows_column(vec![muted_line("nothing carries one yet")])
    } else {
        rows_column(rows)
    };
    section(title, column)
}

/// One bar row: the text bar, then its label.
pub(crate) fn bar_row(label: &str, count: u64, heaviest: u64, ink: gpui::Rgba) -> gpui::AnyElement {
    let fill = bar_fill(count, heaviest);
    let bar: String = "█".repeat(fill);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(ink)
                .child(SharedString::from(bar)),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(label.to_string())),
        )
        .into_any_element()
}

/// A row's bar length: the count's share of the heaviest, on the
/// BAR_CHARS ruler.
fn bar_fill(count: u64, heaviest: u64) -> usize {
    if heaviest == 0 {
        return 0;
    }
    let share = (count as f64 / heaviest as f64) * BAR_CHARS as f64;
    (share.round() as usize).min(BAR_CHARS)
}

/// A section: its muted title, then its body.
pub(crate) fn section(title: &str, body: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(title.to_string())),
        )
        .child(body)
}

/// The rows a section's bars stand in.
pub(crate) fn rows_column(rows: Vec<gpui::AnyElement>) -> gpui::Div {
    div().flex().flex_col().gap_1().children(rows)
}

/// The line an empty section says instead of bars.
fn muted_line(text: &str) -> gpui::AnyElement {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar's fill follows the share of the heaviest, on the ruler,
    /// and never overflows it.
    #[test]
    fn the_bar_fill_follows_the_share() {
        assert_eq!(bar_fill(100, 100), BAR_CHARS);
        assert_eq!(bar_fill(50, 100), BAR_CHARS / 2);
        assert_eq!(bar_fill(0, 100), 0);
        assert_eq!(bar_fill(1, 0), 0, "an empty section has no ruler");
        assert_eq!(bar_fill(200, 100), BAR_CHARS, "the ruler caps the fill");
    }
}
