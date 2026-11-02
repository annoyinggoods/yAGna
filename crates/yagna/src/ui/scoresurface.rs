//! The score surface: the fit spec — editable in place — the
//! catalogue an agent may write against, and the two score
//! distributions side by side.
//!
//! The spec text is the reader's to edit here: the field carries the
//! file's JSON, apply validates it with the same parser the agent's
//! rewrite is refused by and saves it with the backup, and revert
//! puts the file's text back. Its other actions are the re-score
//! (the whole catalog, from the spec) and the folder that carries
//! the spec.
//!
//! Items: `ScoreView` — everything the surface shows; `ScoreActions`
//! — what its parts report; `render` — the surface.
//! Core functions: `fit_labels` — the spread's four quarters;
//! `spec_in_words` — the spec's name, version and parameters as lines;
//! `footer_line` — the last-changed/last-rescore line; `change_line` —
//! what the reloaded spec changed.
//! Depends on: yagna_core::fit (the spec), yagna_store (the
//! distributions), crate::fitspec (the diff), crate::ui::usertab (the
//! bars), crate::ui::field (the spec's editor), crate::ui::theme (the
//! palette).
//! Called by: crate::ui (the window's root entity).

use std::rc::Rc;

use gpui::{App, FontWeight, KeyDownEvent, SharedString, Window, div, prelude::*};
use yagna_core::fit::FitSpec;
use yagna_store::{FitSpread, HealthBuckets};

use super::field::TextField;
use super::rows::RowLayout;
use super::theme;
use super::usertab;

/// A part of the surface asks the pane to do something.
pub(crate) type SurfaceHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The spec field's keystrokes, as the pane handles them.
pub(crate) type SpecKeyHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

/// A click in the spec field seeks the caret to the clicked place.
pub(crate) type SpecSeekHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// What the surface's parts report.
#[derive(Clone)]
pub(crate) struct ScoreActions {
    pub on_rescore: SurfaceHandler,
    pub on_open_folder: SurfaceHandler,
    /// The spec field's keystrokes.
    pub on_spec_key: SpecKeyHandler,
    /// A click into the spec field: the caret seeks the click.
    pub on_spec_seek: SpecSeekHandler,
    /// The apply button: the field's text becomes the spec.
    pub on_apply_spec: SurfaceHandler,
    /// The revert button: the field goes back to the file's text.
    pub on_revert_spec: SurfaceHandler,
}

/// Everything the surface shows, read once per open — and again when
/// the re-score lands.
#[derive(Debug, Clone)]
pub(crate) struct ScoreView {
    /// The spec as pretty JSON — the file's shape, read-only.
    pub spec_text: String,
    /// Where the spec came from: the file, or the default.
    pub source: String,
    /// The loud line when the file fell back to the default.
    pub loud_line: Option<String>,
    /// What the reloaded spec changed since the surface last looked.
    pub change: Option<String>,
    /// What the last action reported — the re-score's counts.
    pub note: Option<String>,
    /// The closed catalogue: each name and its kind.
    pub catalogue: Vec<(String, String)>,
    /// The spec in words: its name and version, then one line per
    /// parameter.
    pub spec_words: Vec<String>,
    /// The health index's four buckets across the catalog.
    pub health: HealthBuckets,
    /// The fit score's four quarters across the catalog.
    pub fit: FitSpread,
    /// The footer's two facts: when the spec last changed, and what the
    /// last re-score did.
    pub footer: String,
    /// Whether a re-score is running.
    pub busy: bool,
}

/// The fit spread's four quarters, labeled and counted, strongest
/// first. The label names the quarter of the row's own maximum — the
/// spec's weights move the scale, so the fraction is the fixed word.
pub(crate) fn fit_labels(spread: &FitSpread) -> [(String, u64); 4] {
    [
        ("75–100% of its maximum".to_string(), spread.strong),
        ("50–75%".to_string(), spread.solid),
        ("25–50%".to_string(), spread.weak),
        ("below 25%".to_string(), spread.poor),
    ]
}

/// The footer: when the spec last changed and what the last re-score
/// covered — each fact saying so when it is not there yet.
pub(crate) fn footer_line(
    spec_changed: Option<&str>,
    last_rescore: Option<(&str, u64, u64)>,
) -> String {
    let changed = match spec_changed {
        Some(when) => format!("the spec last changed {when}"),
        None => "the spec has never been changed here".to_string(),
    };
    let rescored = match last_rescore {
        Some((when, scored, moved)) => {
            format!("the re-score at {when} covered {scored} rows and moved {moved}")
        }
        None => "the catalog has never been re-scored".to_string(),
    };
    format!("{changed} · {rescored}")
}

/// One number as the spec's words say it: whole numbers without the
/// decimal, the rest with one.
fn num(value: f64) -> String {
    if (value - value.trunc()).abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        format!("{value:.1}")
    }
}

/// `point` when the ceiling is exactly one, `points` the rest of the time.
fn points_word(ceiling: f64) -> &'static str {
    if (ceiling - 1.0).abs() < 1e-9 {
        "point"
    } else {
        "points"
    }
}

/// One parameter's rule in words — the tiers, the matches, the wanted
/// topics, or the when-true points.
fn rule_in_words(weight: f64, rule: &yagna_core::fit::FitRule) -> String {
    use yagna_core::fit::FitRule;
    match rule {
        FitRule::Bands { bands } => {
            let tiers: Vec<String> = bands
                .iter()
                .map(|band| match band.max {
                    Some(max) => format!("{} up to {}", num(band.points), num(max)),
                    None => format!("{} beyond", num(band.points)),
                })
                .collect();
            format!(
                "up to {} {}: {}",
                num(weight),
                points_word(weight),
                tiers.join(", ")
            )
        }
        FitRule::MatchRule { values } => {
            let tiers: Vec<String> = values
                .iter()
                .map(|(name, points)| format!("{name} {}", num(*points)))
                .collect();
            format!(
                "up to {} {}: {}",
                num(weight),
                points_word(weight),
                tiers.join(", ")
            )
        }
        FitRule::OverlapRule { overlap, wanted } => {
            let cap = overlap * wanted.len() as f64;
            format!(
                "up to {} {}, {} per wanted topic ({})",
                num(cap),
                points_word(cap),
                num(*overlap),
                wanted.join(", ")
            )
        }
        FitRule::WhenTrueRule { when_true } => format!("{} when true", num(*when_true)),
    }
}

/// The spec in words: its name and version first, then one line per
/// parameter — the weight it may score and the rule that awards it.
pub(crate) fn spec_in_words(spec: &FitSpec) -> Vec<String> {
    let mut lines = vec![format!("{} · version {}", spec.name, spec.version)];
    for param in &spec.parameters {
        lines.push(format!(
            "{} — {}",
            param.name,
            rule_in_words(param.weight, &param.rule)
        ));
    }
    lines
}

/// What the reloaded spec changed since the surface last looked:
/// nothing on a first look, and nothing when nothing moved.
pub(crate) fn change_line(before: Option<&FitSpec>, after: &FitSpec) -> Option<String> {
    let before = before?;
    let touched = crate::fitspec::changed_parameters(before, after);
    if touched.is_empty() {
        return None;
    }
    let noun = if touched.len() == 1 {
        "parameter"
    } else {
        "parameters"
    };
    Some(format!(
        "{} {noun} changed since the last look",
        touched.len()
    ))
}

/// The surface: the header and its two actions, the two distributions
/// side by side, the catalogue, the editable spec, and the footer —
/// one scrollable column, the reader's own page.
pub(crate) fn render(
    view: &ScoreView,
    actions: &ScoreActions,
    spec_field: &TextField,
    window: &mut Window,
) -> gpui::Stateful<gpui::Div> {
    let mut column = div()
        .id("score-surface")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(super::repo::TabLayout::SCROLLBAR)
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .w_full()
        .bg(theme::background())
        .text_color(theme::text());
    column = column.child(render_header(view, actions));
    if let Some(line) = view.loud_line.as_deref() {
        column = column.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::notice_text())
                .child(SharedString::from(line.to_string())),
        );
    }
    if let Some(line) = view.change.as_deref() {
        column = column.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .child(SharedString::from(line.to_string())),
        );
    }
    if let Some(line) = view.note.as_deref() {
        column = column.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .child(SharedString::from(line.to_string())),
        );
    }
    column = column.child(render_distributions(view));
    column = column.child(render_catalogue(view));
    column = column.child(render_spec_words(view));
    column = column.child(render_spec(actions, spec_field, window));
    column = column.child(
        div()
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .child(SharedString::from(view.footer.clone())),
    );
    column
}

/// The header: the surface's name, where the spec came from, and the
/// two actions.
fn render_header(view: &ScoreView, actions: &ScoreActions) -> gpui::Div {
    let rescore = Rc::clone(&actions.on_rescore);
    let open_folder = Rc::clone(&actions.on_open_folder);
    let busy = view.busy;
    let rescore_button = div()
        .id("score-rescore")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| rescore(window, cx))
        .child(if busy {
            "re-scoring…"
        } else {
            "re-score the catalog"
        });
    let folder_button = div()
        .id("score-folder")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| open_folder(window, cx))
        .child("open the folder");
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(RowLayout::top_size())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("score"),
                )
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child(SharedString::from(view.source.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(rescore_button)
                .child(folder_button),
        )
}

/// The two distributions side by side: the pinned health buckets and
/// the fit score's quarters. A catalog nothing has scored yet says so
/// beside the health bars.
fn render_distributions(view: &ScoreView) -> gpui::Div {
    let unscored = view.fit.unscored;
    let total = view.fit.strong + view.fit.solid + view.fit.weak + view.fit.poor;
    let heaviest = view
        .fit
        .strong
        .max(view.fit.solid)
        .max(view.fit.weak)
        .max(view.fit.poor)
        .max(1);
    let rows: Vec<gpui::AnyElement> = fit_labels(&view.fit)
        .into_iter()
        .map(|(label, count)| {
            let label = format!("{label} · {count}");
            usertab::bar_row(&label, count, heaviest, theme::accent())
        })
        .collect();
    let mut fit_column = usertab::section("the fit score's spread", usertab::rows_column(rows));
    if unscored > 0 {
        fit_column = fit_column.child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(format!(
                    "{unscored} of {} rows carry no fit score yet",
                    total + unscored
                ))),
        );
    }
    div()
        .flex()
        .flex_row()
        .gap_6()
        .items_start()
        .child(usertab::health_distribution(&view.health))
        .child(fit_column)
}

/// The closed catalogue: the names an agent may use and the kind of
/// each — the list, not the rules.
fn render_catalogue(view: &ScoreView) -> gpui::Div {
    let entries: Vec<gpui::AnyElement> = view
        .catalogue
        .iter()
        .map(|(name, kind)| {
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text())
                        .child(SharedString::from(name.clone())),
                )
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child(SharedString::from(kind.clone())),
                )
                .into_any_element()
        })
        .collect();
    usertab::section(
        "the catalogue",
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x_4()
            .gap_y_1()
            .children(entries),
    )
}

/// The spec in words: its name and version, then one line per
/// parameter — the human reading of the file below it.
fn render_spec_words(view: &ScoreView) -> gpui::Div {
    let lines: Vec<gpui::AnyElement> = view
        .spec_words
        .iter()
        .map(|line| {
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .child(SharedString::from(line.clone()))
                .into_any_element()
        })
        .collect();
    usertab::section(
        "the spec in words",
        div().flex().flex_col().gap_1().children(lines),
    )
}

/// The spec itself: the field carries the file's JSON in the mono
/// face, apply saves it through the validated, backed-up write, and
/// revert puts the file's text back. Escape reverts too — the field
/// is never silently cleared.
fn render_spec(actions: &ScoreActions, spec_field: &TextField, window: &mut Window) -> gpui::Div {
    let apply = Rc::clone(&actions.on_apply_spec);
    let revert = Rc::clone(&actions.on_revert_spec);
    let key = Rc::clone(&actions.on_spec_key);
    let seek = Rc::clone(&actions.on_spec_seek);
    let apply_button = div()
        .id("spec-apply")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::accent())
        .on_click(move |_, window, cx| apply(window, cx))
        .child("apply the spec");
    let revert_button = div()
        .id("spec-revert")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .on_click(move |_, window, cx| revert(window, cx))
        .child("revert to the file");
    let field = spec_field
        .render_paragraph(
            window,
            move |event, window, cx| key(event, window, cx),
            move |index, window, cx| seek(index, window, cx),
        )
        .font_family(theme::MONO_FONT);
    usertab::section(
        "the spec — the file's shape, editable here",
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(field)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(apply_button)
                    .child(revert_button),
            )
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(
                        "applying validates the JSON and keeps the replaced file at \
                         fit.previous.json — re-score afterwards to move the scores",
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagna_core::fit::default_spec;

    #[test]
    fn the_fit_spread_labels_the_quarters() {
        let spread = FitSpread {
            strong: 1,
            solid: 2,
            weak: 3,
            poor: 4,
            unscored: 5,
        };
        let labels = fit_labels(&spread);
        assert_eq!(labels[0], ("75–100% of its maximum".to_string(), 1));
        assert_eq!(labels[1], ("50–75%".to_string(), 2));
        assert_eq!(labels[2], ("25–50%".to_string(), 3));
        assert_eq!(labels[3], ("below 25%".to_string(), 4));
    }

    #[test]
    fn the_footer_carries_both_facts_or_says_so() {
        let empty = footer_line(None, None);
        assert!(empty.contains("never been changed"), "{empty}");
        assert!(empty.contains("never been re-scored"), "{empty}");
        let line = footer_line(Some("2026-11-02"), Some(("2026-11-03", 412, 3)));
        assert!(line.contains("2026-11-02"), "{line}");
        assert!(line.contains("412 rows"), "{line}");
        assert!(line.contains("moved 3"), "{line}");
    }

    #[test]
    fn the_spec_is_told_in_words() {
        let lines = spec_in_words(&default_spec());
        assert_eq!(lines[0], "default fit · version 1", "{:?}", lines);
        assert_eq!(
            lines[1],
            "recency_days — up to 3 points: 3 up to 90, 1 up to 365, 0 beyond"
        );
        assert_eq!(
            lines[2],
            "license_class — up to 2 points: copyleft 0, permissive 2, unknown 0.5"
        );
        assert_eq!(
            lines[3],
            "health — up to 2 points: 0 up to 4, 1 up to 8, 2 beyond"
        );
        assert_eq!(lines[4], "local — 0.5 when true");
        let mut widened = default_spec();
        widened.version = 4;
        widened.name = "the rewrite".to_string();
        widened.parameters.push(yagna_core::fit::FitParameter {
            name: "topics".to_string(),
            weight: 1.0,
            rule: yagna_core::fit::FitRule::OverlapRule {
                overlap: 0.5,
                wanted: vec!["wasm".to_string(), "parser".to_string()],
            },
        });
        let lines = spec_in_words(&widened);
        assert_eq!(lines[0], "the rewrite · version 4");
        assert_eq!(
            lines.last().expect("the pushed line"),
            "topics — up to 1 point, 0.5 per wanted topic (wasm, parser)"
        );
    }

    #[test]
    fn the_change_line_counts_the_touches() {
        let before = default_spec();
        let mut after = default_spec();
        after.parameters[0].weight += 1.0;
        assert_eq!(
            change_line(Some(&before), &after).as_deref(),
            Some("1 parameter changed since the last look")
        );
        after.parameters[1].weight += 1.0;
        assert_eq!(
            change_line(Some(&before), &after).as_deref(),
            Some("2 parameters changed since the last look")
        );
        assert_eq!(change_line(Some(&before), &before), None);
        assert_eq!(
            change_line(None, &before),
            None,
            "a first look says nothing"
        );
    }
}
