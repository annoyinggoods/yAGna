//! The repository tab: the detail view — the header, the metadata
//! block, the note (one repository's own sentence, above the README),
//! the health table, the license, the folder structure, the rendered
//! README (with its on-demand fetching states), and the digest's
//! placeholder row.
//!
//! Everything the tab shows comes from the store: what is missing is
//! asked for once and marked while it is on its way, never awaited.
//! The tab renders the metadata the sweep carried immediately, and the
//! tree, the README and the images fill in as their queues land.
//!
//! Items: `TabViewActions` — what the tab's interactive parts report;
//! `render_tab` — the tab's element; the section renderers — one per
//! section of the tab.
//! Depends on: yagna_core (the detail record, the health breakdown, the
//! tree shape), yagna_store (the bodies), crate::ui::theme (the
//! palette), crate::ui::readme (the rendered README), crate::ui::tree
//! (the structure), crate::ui::tabs (the tab's state).
//! Called by: crate::ui (the window's root entity).

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use yagna_core::model::RepoDetail;

use super::tabs::{Tab, TabData};
use super::theme;
use super::tree;

/// The tab's own metrics: the sizes of its sections.
pub(crate) struct TabLayout;

impl TabLayout {
    /// The header's name.
    pub(crate) const TITLE: gpui::Pixels = px(22.0);
    /// The section titles and the metadata.
    pub(crate) const BODY: gpui::Pixels = px(13.0);
    /// The labels and the small print.
    pub(crate) const SMALL: gpui::Pixels = px(12.0);
    /// The gap between the sections.
    pub(crate) const GAP: gpui::Pixels = px(18.0);
    /// The width of the label column.
    pub(crate) const LABEL_WIDTH: gpui::Pixels = px(120.0);
    /// The width of the longest language bar.
    pub(crate) const BAR_WIDTH: gpui::Pixels = px(240.0);
    /// The modal's width.
    pub(crate) const MODAL_WIDTH: gpui::Pixels = px(720.0);
    /// The modal's height.
    pub(crate) const MODAL_HEIGHT: gpui::Pixels = px(560.0);
    /// The width of a scrollbar.
    pub(crate) const SCROLLBAR: gpui::Pixels = px(10.0);
    /// The tab's content width: a column of its own, so the text is not
    /// stretched across a wide window.
    pub(crate) const CONTENT_WIDTH: gpui::Pixels = px(980.0);
    /// The height of the digest's view box: the collapsed body opens
    /// into a scroll of its own, never into the whole tab.
    pub(crate) const DIGEST_VIEW: gpui::Pixels = px(420.0);
}

/// A URL leaves the app: the "open on GitHub" action, and the reader's
/// links.
pub(crate) type ExternalHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The license text opens or closes.
pub(crate) type LicenseHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The structure is requested: the tab's "load structure" action.
pub(crate) type StructureHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A directory folds away or opens.
pub(crate) type DirectoryHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The structure drawer opens or closes.
pub(crate) type DrawerHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The README goes to the clipboard.
pub(crate) type CopyHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the tab's interactive parts report.
#[derive(Clone)]
pub(crate) struct TabViewActions {
    /// A URL leaves the app: the "open on GitHub" action, and the
    /// reader's links.
    pub on_external: ExternalHandler,
    /// The license text opens or closes.
    pub on_license: LicenseHandler,
    /// The structure is requested: the tab's "load structure" action.
    pub on_load_structure: StructureHandler,
    /// A directory folds away or opens.
    pub on_toggle_dir: DirectoryHandler,
    /// The license text is fetched.
    pub on_load_license: StructureHandler,
    /// The tags are edited for this repository.
    pub on_edit_tags: StructureHandler,
    /// The note is edited for this repository.
    pub on_edit_note: StructureHandler,
    /// The repository's data is pulled into the cache.
    pub on_prefetch: StructureHandler,
    /// The structure drawer opens or closes.
    pub on_toggle_structure: DrawerHandler,
    /// The README is copied to the clipboard.
    pub on_copy: CopyHandler,
    /// The digest is generated (or regenerated when stale).
    pub on_generate_digest: StructureHandler,
    /// The digest's body is shown or folded away.
    pub on_toggle_digest: StructureHandler,
    /// The digest is copied to the clipboard.
    pub on_copy_digest: CopyHandler,
    /// The digest is saved to a `.txt` file.
    pub on_save_digest: CopyHandler,
}

/// The tab: the sections, in a scroll of their own. The tab remembers
/// where it was scrolled to, so coming back to it lands where it was
/// left.
pub(crate) fn render_tab(tab: &Tab, actions: &TabViewActions) -> gpui::Stateful<gpui::Div> {
    let mut column = div().flex().flex_col().gap(TabLayout::GAP).p_4().w_full();
    column = column.child(render_header(tab, actions));
    let Some(data) = tab.data.as_ref() else {
        column = column.child(pending_line("the tab is loading…"));
        return scrollable(column, tab);
    };
    let Some(detail) = data.detail.as_ref() else {
        column = column.child(pending_line("this repository is not in the catalog"));
        return scrollable(column, tab);
    };
    column = column.child(render_metadata(detail));
    column = column.child(render_tags(tab, actions));
    let handler = Rc::clone(&actions.on_edit_note);
    column = column.child(render_note_section(
        &format!("repo-{}", tab.rowid),
        data.note.as_deref(),
        &handler,
    ));
    column = column.child(render_scores(detail));
    column = column.child(render_license(detail, tab, actions));
    column = column.child(render_structure(detail, tab, data, actions));
    column = column.child(render_readme(tab, data, actions));
    column = column.child(render_digest(tab, actions));
    let mut root = scrollable(column, tab);
    if tab.license_open {
        let body = data.license.clone().unwrap_or_default();
        root = root.child(render_license_modal(&body, actions));
    }
    root
}

/// The scroll container: the tab's own scroll, remembered by its handle.
fn scrollable(column: gpui::Div, tab: &Tab) -> gpui::Stateful<gpui::Div> {
    div()
        .id("tab-scroll")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(TabLayout::SCROLLBAR)
        .track_scroll(&tab.scroll)
        .child(
            div()
                .w_full()
                .max_w(TabLayout::CONTENT_WIDTH)
                .mx_auto()
                .px_4()
                .py_4()
                .child(column),
        )
}

/// The header: the repository's name in capitals, the forge's chip, and
/// the action that opens the repository on the service.
fn render_header(tab: &Tab, actions: &TabViewActions) -> gpui::Div {
    let title = format!("{}/{}", tab.repo.owner, tab.repo.name).to_uppercase();
    let url = format!("https://github.com/{}/{}", tab.repo.owner, tab.repo.name);
    let handler = Rc::clone(&actions.on_external);
    let target = url.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_size(TabLayout::TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(SharedString::from(title)),
                )
                .child(render_chip(tab.repo.forge.as_str())),
        )
        .child(
            div()
                .id("pull-everything")
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .on_click({
                    let handler = Rc::clone(&actions.on_prefetch);
                    move |_, window, cx| handler(window, cx)
                })
                .child("pull everything"),
        )
        .child(
            div()
                .id("open-on-github")
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .on_click(move |_, window, cx| handler(&target, window, cx))
                .child("open on github"),
        )
}

/// The forge's chip: present from day one, inert until the forges
/// multiply.
fn render_chip(forge: &str) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(theme::surface_alt())
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(SharedString::from(forge.to_string()))
}

/// The metadata block: the languages with their bars, the numbers, the
/// topics, and the dates.
fn render_metadata(detail: &RepoDetail) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(render_section_title("metadata"));
    column = column.child(render_languages(detail));
    column = column.child(render_numbers(detail));
    column = column.child(render_topics(detail));
    column = column.child(render_dates(detail));
    column
}

/// The languages: each one with a bar in its own color.
fn render_languages(detail: &RepoDetail) -> gpui::Div {
    if detail.languages.is_empty() {
        return div();
    }
    let largest = detail
        .languages
        .iter()
        .map(|language| language.bytes)
        .max()
        .unwrap_or(1)
        .max(1);
    let rows: Vec<AnyElement> = detail
        .languages
        .iter()
        .map(|language| {
            let share = language.bytes as f64 / largest as f64;
            let width = TabLayout::BAR_WIDTH * share as f32;
            let percent = (language.bytes as f64 / largest as f64 * 100.0).round() as u64;
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(90.0))
                        .text_size(TabLayout::SMALL)
                        .text_color(theme::text())
                        .child(SharedString::from(language.name.clone())),
                )
                .child(
                    div()
                        .w(TabLayout::BAR_WIDTH)
                        .h(px(8.0))
                        .rounded_sm()
                        .bg(theme::surface_alt())
                        .child(
                            div()
                                .w(width)
                                .h_full()
                                .rounded_sm()
                                .bg(theme::data_color(language.color.as_deref())),
                        ),
                )
                .child(
                    div()
                        .text_size(TabLayout::SMALL)
                        .text_color(theme::text_muted())
                        .child(SharedString::from(format!("{percent}%"))),
                )
                .into_any_element()
        })
        .collect();
    div().flex().flex_col().gap_1().children(rows)
}

/// The numbers: the stars, the health, the fit, the commits, the
/// release — in that order, each carrying its name.
fn numbers_pieces(detail: &RepoDetail) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    pieces.push(format!("{} stars", detail.stars));
    pieces.push(format!("health {}/12", detail.health.total));
    pieces.push(match detail.fit.as_ref() {
        Some(fit) => format!("fit {:.1}", fit.total),
        None => "fit —".to_string(),
    });
    pieces.push(format!("{} commits", detail.commit_count));
    if let Some(release) = detail.latest_release.as_ref() {
        pieces.push(release.tag.clone());
    }
    pieces
}

fn render_numbers(detail: &RepoDetail) -> gpui::Div {
    div()
        .text_size(TabLayout::BODY)
        .text_color(theme::text())
        .child(SharedString::from(numbers_pieces(detail).join("  ·  ")))
}

/// The topics: the repository's own words.
fn render_topics(detail: &RepoDetail) -> gpui::Div {
    if detail.topics.is_empty() {
        return div();
    }
    let chips: Vec<AnyElement> = detail
        .topics
        .iter()
        .map(|topic| {
            div()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(theme::surface_alt())
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .child(SharedString::from(topic.clone()))
                .into_any_element()
        })
        .collect();
    div().flex().flex_row().flex_wrap().gap_1().children(chips)
}

/// The dates: the creation, the first commit and where it came from,
/// the last commit, and the star.
fn render_dates(detail: &RepoDetail) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_fact("created", &date_of(detail.created_at)));
    if let Some(date) = detail.first_commit_date {
        column = column.child(render_fact(
            "first commit",
            &format!(
                "{date} ({})",
                source_label(detail.first_commit_source.as_deref())
            ),
        ));
        column = column.child(render_caveat());
    }
    column = column.child(render_fact("last commit", &date_of(detail.last_commit_at)));
    if let Some(starred) = detail.starred_at {
        column = column.child(render_fact("starred", &date_of(starred)));
    }
    column
}

/// One dated fact: its label and its value.
fn render_fact(label: &str, value: &str) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .child(
            div()
                .w(TabLayout::LABEL_WIDTH)
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .text_size(TabLayout::BODY)
                .text_color(theme::text())
                .child(SharedString::from(value.to_string())),
        )
}

/// The caveat the first commit's date carries: the date is provenance,
/// not a precision.
fn render_caveat() -> gpui::Div {
    div()
        .pl(TabLayout::LABEL_WIDTH + px(12.0))
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(
            "history imports predate the repository; the default branch only; rebases shift things",
        )
}

/// Where the first commit's date came from, in words.
fn source_label(source: Option<&str>) -> &'static str {
    match source {
        Some("git-history") => "from git history",
        _ => "from repo creation",
    }
}

/// A date, as the catalog stores it.
fn date_of(date: chrono::DateTime<chrono::Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// The note: one repository's own words, above the README, with the
/// way in. The catalog's tab and the local tab show the same section —
/// one rule, one section — and the editor is a popup over the page.
pub(crate) fn render_note_section(
    id: &str,
    note: Option<&str>,
    on_edit: &StructureHandler,
) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("note"));
    let handler = Rc::clone(on_edit);
    let edit = div()
        .id(SharedString::from(format!("tab-note-edit-{id}")))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(TabLayout::SMALL)
        .text_color(theme::accent())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            handler(window, cx);
        })
        .child("edit");
    let body = match note {
        Some(note) => div()
            .flex_1()
            .min_w_0()
            .text_size(TabLayout::BODY)
            .text_color(theme::text())
            .child(SharedString::from(note.to_string())),
        None => div()
            .flex_1()
            .min_w_0()
            .text_size(TabLayout::BODY)
            .text_color(theme::text_muted())
            .child("no note"),
    };
    column = column.child(
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_2()
            .child(body)
            .child(edit),
    );
    column
}

/// The repository's own tags: the reader's names for it. The button
/// opens the sidebar's editor for this repository — the tags are the
/// reader's, wherever they are edited.
fn render_tags(tab: &Tab, actions: &TabViewActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("tags"));
    let mut chips: Vec<gpui::AnyElement> = tab
        .data
        .as_ref()
        .map(|data| data.tags.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|tag| {
            div()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(theme::surface_alt())
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .child(SharedString::from(tag))
                .into_any_element()
        })
        .collect();
    chips.push(
        div()
            .id(SharedString::from(format!("tab-tags-{}", tab.rowid)))
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(theme::surface_alt())
            .text_size(TabLayout::SMALL)
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .on_click({
                let handler = Rc::clone(&actions.on_edit_tags);
                move |_, window, cx| handler(window, cx)
            })
            .child("+")
            .into_any_element(),
    );
    column = column.child(div().flex().flex_row().flex_wrap().gap_1().children(chips));
    column
}

/// The two score tables side by side: the pinned health criteria and
/// the spec's fit parameters. A row no spec has scored yet shows the
/// health table alone and one line saying so.
fn render_scores(detail: &RepoDetail) -> gpui::Div {
    let pair = div().flex().flex_row().gap_6().items_start();
    let pair = pair.child(render_health(detail));
    match &detail.fit {
        Some(fit) => pair.child(render_fit(fit)),
        None => pair.child(render_fit_unscored()),
    }
}

/// The fit table: the spec's parameters, the points they scored, and
/// the weight they could score.
fn render_fit(fit: &yagna_core::fit::FitReport) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("fit"));
    let rows: Vec<AnyElement> = fit
        .parameters
        .iter()
        .map(|param| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(TabLayout::LABEL_WIDTH)
                        .text_size(TabLayout::SMALL)
                        .text_color(theme::text_muted())
                        .child(SharedString::from(param.name.clone())),
                )
                .child(
                    div()
                        .text_size(TabLayout::BODY)
                        .text_color(theme::text())
                        .child(SharedString::from(format!(
                            "{:.1}/{:.1}",
                            param.points, param.weight
                        ))),
                )
                .into_any_element()
        })
        .collect();
    column = column.children(rows);
    column = column.child(
        div()
            .flex()
            .flex_row()
            .gap_3()
            .mt_1()
            .child(
                div()
                    .w(TabLayout::LABEL_WIDTH)
                    .text_size(TabLayout::SMALL)
                    .text_color(theme::text_muted())
                    .child("total"),
            )
            .child(
                div()
                    .text_size(TabLayout::BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent())
                    .child(SharedString::from(format!(
                        "{:.1}/{:.1}",
                        fit.total, fit.maximum
                    ))),
            ),
    );
    if fit.version > 0 {
        column = column.child(
            div()
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child(SharedString::from(format!(
                    "scored by spec version {}",
                    fit.version
                ))),
        );
    }
    column
}

/// The fit table's stand-in: the one line that says no spec has scored
/// this row yet.
fn render_fit_unscored() -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("fit"));
    column.child(pending_line(
        "no score yet — re-score from the score surface",
    ))
}

/// The health table: the criteria, their points, and the points they
/// could score.
fn render_health(detail: &RepoDetail) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("health"));
    let rows: Vec<AnyElement> = detail
        .health
        .breakdown()
        .iter()
        .map(|(name, scored, maximum)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(TabLayout::LABEL_WIDTH)
                        .text_size(TabLayout::SMALL)
                        .text_color(theme::text_muted())
                        .child(SharedString::from((*name).to_string())),
                )
                .child(
                    div()
                        .text_size(TabLayout::BODY)
                        .text_color(theme::text())
                        .child(SharedString::from(format!("{scored}/{maximum}"))),
                )
                .into_any_element()
        })
        .collect();
    column = column.child(
        div()
            .flex()
            .flex_row()
            .gap_3()
            .mt_1()
            .child(
                div()
                    .w(TabLayout::LABEL_WIDTH)
                    .text_size(TabLayout::SMALL)
                    .text_color(theme::text_muted())
                    .child("total"),
            )
            .child(
                div()
                    .text_size(TabLayout::BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent())
                    .child(SharedString::from(format!("{}/12", detail.health.total))),
            ),
    );
    column = column.children(rows);
    column
}

/// The license: its name, and — for a license that is not one of the
/// SPDX ones — the action that opens the stored text.
///
/// "NOASSERTION" is the service's way of saying *there is a license,
/// and it is not one of the standard ones*: it is a non-standard
/// license, and it gets the same treatment as a missing SPDX id.
fn render_license(detail: &RepoDetail, tab: &Tab, actions: &TabViewActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("license"));
    let license = detail.license.as_ref();
    let standard = license
        .and_then(|license| license.spdx_id.as_deref())
        .is_some_and(|id| id != NOASSERTION);
    let name = license
        .map(|license| license.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "a custom license".to_string());
    if standard {
        column = column.child(render_value(&name));
        return column;
    }
    column = column.child(render_value(&name));
    let has_text = tab
        .data
        .as_ref()
        .and_then(|data| data.license.as_ref())
        .is_some();
    if !has_text {
        let handler = Rc::clone(&actions.on_load_license);
        column = column.child(
            div()
                .id("fetch-license")
                .mt_1()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .on_click(move |_, window, cx| handler(window, cx))
                .child("fetch the license text"),
        );
        return column;
    }
    let handler = Rc::clone(&actions.on_license);
    column = column.child(
        div()
            .id("view-license")
            .mt_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(TabLayout::SMALL)
            .text_color(theme::accent())
            .on_click(move |_, window, cx| handler(true, window, cx))
            .child("view text"),
    );
    column
}

/// The service's word for "there is a license, and it is not one of the
/// SPDX ones".
const NOASSERTION: &str = "NOASSERTION";

/// The structure: a drawer over the stored tree, closed by default —
/// a big repository's tree is tens of thousands of entries, and it does
/// not belong between the reader and the README.
fn render_structure(
    detail: &RepoDetail,
    tab: &Tab,
    data: &TabData,
    actions: &TabViewActions,
) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    let open = tab.structure_open;
    column = column.child(render_drawer("structure", open, actions));
    if !open {
        return column;
    }
    let Some(body) = tab.data.as_ref().and_then(|data| data.tree.as_ref()) else {
        let handler = Rc::clone(&actions.on_load_structure);
        let label = if detail.default_branch.is_some() {
            "load structure"
        } else {
            "no default branch — there is no structure"
        };
        let actionable = detail.default_branch.is_some();
        return column.child(
            div()
                .id("load-structure")
                .mt_1()
                .px_2()
                .py_1()
                .rounded_sm()
                .text_size(TabLayout::SMALL)
                .text_color(if actionable {
                    theme::accent()
                } else {
                    theme::text_muted()
                })
                .when(actionable, |action| {
                    action
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_alt()))
                        .on_click(move |_, window, cx| handler(window, cx))
                })
                .child(label),
        );
    };
    // The shape was parsed once, when the tab's data landed — the
    // render path never parses (a big tree is tens of thousands of
    // entries, and a parse per frame would grind the window).
    let nodes = &data.tree_nodes;
    let handler = Rc::clone(&actions.on_toggle_dir);
    let rows = tree::render_tree(nodes, &tab.collapsed, &handler);
    column = column.child(div().font_family(theme::MONO_FONT).children(rows));
    let total = tree::count_entries(nodes);
    if total > tree::TREE_ENTRY_CAP {
        column = column.child(render_note(&format!(
            "… and {} more entries",
            total - tree::TREE_ENTRY_CAP
        )));
    }
    if body.truncated {
        column = column.child(render_note("the forge cut the structure short"));
    }
    column
}

/// The drawer's header: the section's name and its state. Clicking it
/// opens or closes the drawer.
fn render_drawer(title: &str, open: bool, actions: &TabViewActions) -> gpui::Stateful<gpui::Div> {
    let handler = Rc::clone(&actions.on_toggle_structure);
    div()
        .id(SharedString::from(format!("drawer-{title}")))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .mt_2()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |_, window, cx| handler(window, cx))
        .child(
            div()
                .text_size(TabLayout::SMALL)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(SharedString::from(title.to_uppercase())),
        )
        .child(
            div()
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child(SharedString::from(if open { "▾" } else { "▸" })),
        )
}

/// The README: the stored body, rendered. The blocks were parsed when
/// the tab's data landed — the render path never parses. A README the
/// tab is fetching on demand says so; one that could not be fetched
/// says why in one line and never reads as an empty README.
fn render_readme(tab: &Tab, data: &super::tabs::TabData, actions: &TabViewActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_2();
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(TabLayout::SMALL)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("README"),
        );
    let handler = Rc::clone(&actions.on_copy);
    let copy = div()
        .id("copy-readme")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(TabLayout::SMALL)
        .text_color(theme::accent())
        .on_click(move |_, window, cx| handler(window, cx))
        .child("copy");
    column = column.child(header.child(copy));
    if data.readme.is_none() {
        if tab.readme_fetching {
            return column.child(pending_line("fetching the readme…"));
        }
        if let Some(why) = data.readme_note.as_deref() {
            return column.child(render_note_line(&format!(
                "the readme could not be fetched — {why}"
            )));
        }
        return column.child(render_note_line("no readme has been fetched yet"));
    }
    let handler = Rc::clone(&actions.on_external);
    let elements = super::readme::render_blocks(&data.blocks, &handler);
    column = column.child(div().flex().flex_col().gap_2().children(elements));
    if data.readme.as_ref().is_some_and(|readme| readme.truncated) {
        column = column.child(render_note("the readme was cut at the store's cap"));
    }
    column
}

/// A one-line note about the README, in the muted ink — a fact, never
/// an alarm.
fn render_note_line(text: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

/// The digest: the collapsed gitingest. A repository with no digest
/// carries the Generate action; a digest carries its provenance line,
/// the collapsed body, and the actions that take it away. A digest the
/// sweep marked stale says so and offers to regenerate — the fresh
/// open regenerates exactly once.
fn render_digest(tab: &Tab, actions: &TabViewActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_section_title("digest"));
    let Some(data) = tab.data.as_ref() else {
        return column;
    };
    let Some(digest) = data.digest.as_ref() else {
        return column.child(render_digest_generate(data, actions));
    };
    let taken = digest.taken_at.format("%Y-%m-%d").to_string();
    column = column.child(render_digest_status(digest, &taken, actions));
    let mut chips = div().flex().flex_row().gap_2();
    chips = chips.child(render_digest_action(
        "digest-view",
        if tab.digest_open { "hide" } else { "view" },
        actions.on_toggle_digest.clone(),
    ));
    chips = chips.child(render_digest_action(
        "digest-copy",
        "copy",
        actions.on_copy_digest.clone(),
    ));
    chips = chips.child(render_digest_action(
        "digest-save",
        "save to file",
        actions.on_save_digest.clone(),
    ));
    if digest.stale_at.is_some() {
        chips = chips.child(render_digest_action(
            "digest-regenerate",
            "regenerate",
            actions.on_generate_digest.clone(),
        ));
    }
    column = column.child(chips);
    if !tab.digest_open {
        return column;
    }
    column.child(
        div()
            .id("digest-body")
            .mt_1()
            .h(TabLayout::DIGEST_VIEW)
            .overflow_y_scroll()
            .scrollbar_width(TabLayout::SCROLLBAR)
            .font_family(theme::MONO_FONT)
            .text_size(TabLayout::SMALL)
            .text_color(theme::text())
            .child(SharedString::from(digest.body.clone())),
    )
}

/// The no-digest state: the Generate action when the repository has a
/// branch to digest, the quiet fact when it has none.
fn render_digest_generate(data: &super::tabs::TabData, actions: &TabViewActions) -> gpui::Div {
    let branched = data
        .detail
        .as_ref()
        .and_then(|detail| detail.default_branch.as_deref())
        .is_some();
    let mut column = div().flex().flex_col().gap_1();
    if !branched {
        return column.child(pending_line("no default branch — nothing to digest"));
    }
    let handler = Rc::clone(&actions.on_generate_digest);
    column = column.child(
        div()
            .id("digest-generate")
            .px_2()
            .py_1()
            .mr_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(TabLayout::SMALL)
            .text_color(theme::accent())
            .on_click(move |_, window, cx| handler(window, cx))
            .child("generate"),
    );
    column.child(pending_line(
        "the digest is taken on demand and cached until the repository moves",
    ))
}

/// The digest's provenance line: when it was taken, how big the token
/// estimate says it is, whether the caps cut it — and, when the sweep
/// marked it stale, the regenerate offer the roadmap words verbatim.
fn render_digest_status(
    digest: &yagna_store::DigestBody,
    taken: &str,
    actions: &TabViewActions,
) -> gpui::Div {
    let mut line = div().flex().flex_row().items_center().gap_1();
    let tokens = digest
        .token_estimate
        .map(yagna_core::digest::format_tokens)
        .unwrap_or_else(|| "an unknown number of".to_string());
    let mut text = format!("digest of {taken} — {tokens} tokens");
    if digest.truncated {
        text.push_str(" — cut at the caps");
    }
    line = line.child(
        div()
            .text_size(TabLayout::SMALL)
            .text_color(theme::text_muted())
            .child(SharedString::from(text)),
    );
    if digest.stale_at.is_some() {
        let handler = Rc::clone(&actions.on_generate_digest);
        line = line.child(
            div()
                .text_size(TabLayout::SMALL)
                .text_color(theme::text_muted())
                .child("—"),
        );
        line = line.child(
            div()
                .id("digest-stale")
                .px_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(TabLayout::SMALL)
                .text_color(theme::accent())
                .on_click(move |_, window, cx| handler(window, cx))
                .child("regenerate"),
        );
    }
    line
}

/// One digest action chip: view, copy, save, regenerate.
fn render_digest_action(
    id: &str,
    label: &str,
    handler: StructureHandler,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id.to_string()))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(TabLayout::SMALL)
        .text_color(theme::accent())
        .on_click(move |_, window, cx| handler(window, cx))
        .child(SharedString::from(label.to_string()))
}

/// The license modal: the stored text, scrollable, over the tab.
fn render_license_modal(body: &str, actions: &TabViewActions) -> gpui::Stateful<gpui::Div> {
    let handler = Rc::clone(&actions.on_license);
    div()
        .id("license-modal")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(theme::scrim())
        .on_click(move |_, window, cx| handler(false, window, cx))
        .child(
            div()
                .id("license-panel")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("license-text")
                        .w(TabLayout::MODAL_WIDTH)
                        .h(TabLayout::MODAL_HEIGHT)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_4()
                        .rounded_sm()
                        .bg(theme::surface())
                        .border_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(TabLayout::BODY)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child("the license file, as it is stored"),
                                )
                                .child(
                                    div()
                                        .id("license-close")
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_alt()))
                                        .text_size(TabLayout::SMALL)
                                        .text_color(theme::accent())
                                        .on_click({
                                            let handler = Rc::clone(&actions.on_license);
                                            move |_, window, cx| handler(false, window, cx)
                                        })
                                        .child("close"),
                                ),
                        )
                        .child(
                            div()
                                .id("license-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .scrollbar_width(TabLayout::SCROLLBAR)
                                .font_family(theme::MONO_FONT)
                                .text_size(TabLayout::SMALL)
                                .text_color(theme::text())
                                .child(SharedString::from(body.to_string())),
                        ),
                ),
        )
}

/// A plain value under a section's title.
fn render_value(value: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::BODY)
        .text_color(theme::text())
        .child(SharedString::from(value.to_string()))
}

/// A note under a section: the small print, in the muted ink.
fn render_note(text: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

/// A section's title.
fn render_section_title(title: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(SharedString::from(title.to_uppercase()))
}

/// The one line that says what is not there yet.
fn pending_line(text: &str) -> gpui::Div {
    div()
        .text_size(TabLayout::SMALL)
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use yagna_core::health::HealthReport;
    use yagna_core::model::{Forge, LicenseInfo, RepoRef};

    fn instant(year: i32, month: u32, day: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("date")
    }

    fn bare_detail() -> RepoDetail {
        RepoDetail {
            repo_rowid: 1,
            repo_ref: RepoRef {
                forge: Forge::GitHub,
                owner: "someone".to_string(),
                name: "something".to_string(),
            },
            description: None,
            stars: 42,
            languages: Vec::new(),
            license: Some(LicenseInfo {
                name: "MIT License".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: Vec::new(),
            default_branch: Some("main".to_string()),
            created_at: instant(2023, 4, 1),
            last_commit_at: instant(2026, 9, 30),
            starred_at: Some(instant(2026, 10, 1)),
            first_commit_date: Some(instant(2023, 4, 1)),
            first_commit_source: Some("repo-created".to_string()),
            commit_count: 100,
            latest_release: None,
            is_fork: false,
            is_archived: false,
            health: HealthReport::default(),
            fit: None,
        }
    }

    #[test]
    fn the_numbers_carry_the_fit_beside_the_health() {
        let pieces = numbers_pieces(&bare_detail());
        assert_eq!(pieces[0], "42 stars");
        assert_eq!(pieces[1], "health 0/12");
        assert_eq!(pieces[2], "fit —", "an unscored row says so");
        assert_eq!(pieces[3], "100 commits");

        let mut scored = bare_detail();
        scored.fit = Some(yagna_core::fit::FitReport {
            version: 2,
            total: 4.2,
            maximum: 7.5,
            parameters: Vec::new(),
        });
        let pieces = numbers_pieces(&scored);
        assert_eq!(pieces[2], "fit 4.2");
    }
}
