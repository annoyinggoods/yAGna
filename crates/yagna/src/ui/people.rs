//! The PEOPLE tab: the followed users and their read-only shadows.
//!
//! A followed login is a drawer: closed, it costs one line and says
//! how many repositories it holds; open, it shows that person's
//! repositories as a clickable list (a click opens the repository as a
//! shadow tab), with the per-user refresh, the unfollow and the last
//! fetch's stamp beside them. A refresh that fails leaves the last
//! fetch standing and says so: offline shows the last fetch. Shadow
//! rows never enter the catalog's surfaces; they are browsable and
//! that is all.
//!
//! The tab also suggests who to follow: the logins the token's own
//! user follows, less the ones this catalog follows, with a line that
//! says where the suggestions came from and when. A failed fetch says
//! why and leaves the type-a-login path standing.
//!
//! Items: `PeopleState` — the shadows, stamps, the open drawer and the
//! suggestions; `PersonActions` — one person's drawer actions;
//! `FollowHandler` — what a suggestion's follow action calls;
//! `triple_line` — the pitch's triple as one line; `show_stamp` — a
//! stored stamp as the reader reads it.
//! Depends on: yagna_core (the shadows), gpui (the elements),
//! chrono (the stamp's shape).
//! Called by: crate::ui (the list pane, PEOPLE tab in front).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, ClickEvent, SharedString, Window, div, prelude::*};
use yagna_core::model::{RepoSnapshot, ShadowRepo};

use super::rows::RowLayout;
use super::theme;

/// How many suggestions the block shows at most.
const MAX_SUGGESTIONS: usize = 12;

/// How many of one person's repositories the open drawer lists — the
/// shadows hold at most 500, and the drawer scrolls in its own right.
const PERSON_REPOSITORY_CAP: usize = 200;

/// The heading of the first block: the people this catalog follows,
/// and how many.
pub(crate) fn following_heading(count: usize) -> String {
    format!("Following ({count})")
}

/// The heading of the second block: the way to find more.
pub(crate) const FIND_MORE: &str = "Find more";

/// What a suggestion's follow action calls: the login to follow rides
/// along, and the callback does the following.
pub(crate) type FollowHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A repository row was clicked: the snapshot rides along, and the
/// callback opens its shadow tab.
pub(crate) type OpenRepoHandler = Rc<dyn Fn(RepoSnapshot, &mut Window, &mut App)>;

/// One person's drawer was toggled: the login rides along.
pub(crate) type PersonToggleHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// One person's plain action (refresh, unfollow): the login rides
/// along.
pub(crate) type PersonActionHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What one person's drawer reports.
#[derive(Clone)]
pub(crate) struct PersonActions {
    /// The drawer was opened or closed.
    pub on_toggle: PersonToggleHandler,
    /// The person's stars are refreshed — per user, never automatic.
    pub on_refresh: PersonActionHandler,
    /// The person is unfollowed.
    pub on_unfollow: PersonActionHandler,
    /// One of the person's repositories was clicked: it opens as a
    /// shadow tab.
    pub on_open_repo: OpenRepoHandler,
}

/// The PEOPLE tab's state: one shadow list per login, with its stamp,
/// whichever refresh is in flight, and the drawer that is open.
#[derive(Debug, Default)]
pub(crate) struct PeopleState {
    /// One login's shadows, most stars first.
    pub shadows: HashMap<String, Vec<ShadowRepo>>,
    /// One login's last-fetch stamp, as the store wrote it.
    pub stamps: HashMap<String, String>,
    /// Which login's drawer is open, when one is.
    pub open_drawer: Option<String>,
    /// Which login is refreshing now, when one is.
    pub refreshing: Option<String>,
    /// What the tab last said, when it said anything.
    pub note: Option<String>,
    /// The logins the reader could follow: what the forge named, less
    /// the ones already followed.
    pub suggestions: Vec<String>,
    /// When the suggestions last landed, as the reader reads it.
    pub suggestion_stamp: Option<String>,
    /// Why the suggestions are missing, when the fetch failed.
    pub suggestion_failure: Option<String>,
}

impl PeopleState {
    /// Records one login's refresh: the shadows and the stamp the
    /// store wrote.
    pub(crate) fn apply_refresh(&mut self, login: &str, shadows: Vec<ShadowRepo>) {
        let stamp = shadows
            .first()
            .map(|shadow| shadow.fetched_at.to_rfc3339())
            .unwrap_or_default();
        if !stamp.is_empty() {
            self.stamps.insert(login.to_string(), stamp);
        }
        self.shadows.insert(login.to_string(), shadows);
        if self.refreshing.as_deref() == Some(login) {
            self.refreshing = None;
        }
    }

    /// Drops one login's views: unfollowing takes the shadows with it.
    pub(crate) fn drop_login(&mut self, login: &str) {
        self.shadows.remove(login);
        self.stamps.remove(login);
        if self.refreshing.as_deref() == Some(login) {
            self.refreshing = None;
        }
        if self.open_drawer.as_deref() == Some(login) {
            self.open_drawer = None;
        }
    }

    /// Opens or closes one login's drawer.
    pub(crate) fn toggle_drawer(&mut self, login: &str) {
        self.open_drawer = if self.open_drawer.as_deref() == Some(login) {
            None
        } else {
            Some(login.to_string())
        };
    }

    /// The logins the reader could follow: what the forge answered,
    /// less the ones already followed — de-duplicated, the forge's
    /// order kept, and capped.
    pub(crate) fn suggestions_for(logins: &[String], followed: &[String]) -> Vec<String> {
        let held: HashSet<&str> = followed.iter().map(|login| login.trim()).collect();
        let mut seen: HashSet<String> = HashSet::new();
        logins
            .iter()
            .map(|login| login.trim())
            .filter(|login| !login.is_empty() && !held.contains(*login))
            .filter(|login| seen.insert((*login).to_string()))
            .take(MAX_SUGGESTIONS)
            .map(|login| login.to_string())
            .collect()
    }

    /// Folds the forge's answer into the state: the logins the reader
    /// could follow, the moment they landed, and no failure any more.
    pub(crate) fn apply_suggestions(&mut self, logins: &[String], followed: &[String]) {
        self.suggestions = Self::suggestions_for(logins, followed);
        self.suggestion_failure = None;
        self.suggestion_stamp = Some(show_stamp(&chrono::Utc::now().to_rfc3339()));
    }

    /// Records that the suggestions could not be fetched: the reason
    /// rides along, and the block says why it has no rows.
    pub(crate) fn record_suggestion_failure(&mut self, reason: &str) {
        self.suggestions.clear();
        self.suggestion_failure = Some(reason.to_string());
    }

    /// Drops one suggestion: the login was followed, so it is not a
    /// suggestion any more.
    pub(crate) fn remove_suggestion(&mut self, login: &str) {
        self.suggestions.retain(|held| held != login);
    }

    /// The note the suggestions block shows beside its heading: where
    /// the suggestions came from and when — or, offline, the last fetch
    /// and the reason this one did not land.
    pub(crate) fn suggestion_note(&self) -> String {
        match (&self.suggestion_failure, &self.suggestion_stamp) {
            (Some(reason), Some(stamp)) => {
                format!("offline — the last fetch was {stamp} ({reason})")
            }
            (Some(reason), None) => format!("offline — {reason}"),
            (None, Some(stamp)) => format!("from your GitHub following, fetched {stamp}"),
            (None, None) => "from your GitHub following".to_string(),
        }
    }
}

/// The pitch's triple as one line: name, languages, stars — nothing
/// more.
pub(crate) fn triple_line(shadow: &ShadowRepo) -> String {
    let (name, languages, stars) = shadow.triple();
    let languages = bare_languages(&languages);
    format!("{name} · {languages} · {stars} stars")
}

/// The triple's language slice: the first two, or the em dash when
/// the service named none.
fn bare_languages(languages: &[String]) -> String {
    if languages.is_empty() {
        return "—".to_string();
    }
    let mut shown: Vec<&str> = languages.iter().take(2).map(String::as_str).collect();
    if languages.len() > 2 {
        shown.push("…");
    }
    shown.join(", ")
}

/// A stored stamp as the reader reads it: `YYYY-MM-DD HH:MM`. A stamp
/// that does not parse shows as it was stored — never as nothing.
pub(crate) fn show_stamp(stamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .map(|moment| moment.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| stamp.to_string())
}

/// One person's drawer: the header line (the login, the count, the
/// chevron) and, when open, the actions and the clickable repositories.
pub(crate) fn render_person(
    login: &str,
    shadows: &[ShadowRepo],
    stamp: Option<&str>,
    open: bool,
    actions: &PersonActions,
) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(render_person_header(
        login,
        shadows.len(),
        stamp,
        open,
        actions,
    ));
    if open {
        column = column.child(render_person_body(login, shadows, actions));
    }
    column
}

/// The drawer's header: the login, how many repositories it holds, the
/// last fetch's stamp, and the chevron that says which way it goes.
fn render_person_header(
    login: &str,
    count: usize,
    stamp: Option<&str>,
    open: bool,
    actions: &PersonActions,
) -> gpui::Stateful<gpui::Div> {
    let on_toggle = Rc::clone(&actions.on_toggle);
    let stamp = stamp
        .map(show_stamp)
        .unwrap_or_else(|| "never fetched".to_string());
    let login_text = login.to_string();
    div()
        .id(SharedString::from(format!("people-header-{login}")))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(&login_text, window, cx);
        })
        .child(
            div()
                .text_size(RowLayout::top_size())
                .text_color(theme::accent())
                .child(SharedString::from(login.to_string())),
        )
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(format!(
                    "{count} repositories · fetched {stamp}"
                ))),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(if open { "▾" } else { "▸" })),
        )
}

/// The drawer's open body: the per-user actions, then the repositories
/// as a clickable list.
fn render_person_body(login: &str, shadows: &[ShadowRepo], actions: &PersonActions) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1().px_3().py_1();
    column = column.child(render_person_actions(login, actions));
    let rows: Vec<gpui::AnyElement> = shadows
        .iter()
        .take(PERSON_REPOSITORY_CAP)
        .map(|shadow| render_person_repo(shadow, actions))
        .collect();
    column = column.children(rows);
    if shadows.len() > PERSON_REPOSITORY_CAP {
        column = column.child(muted_line(&format!(
            "… and {} more",
            shadows.len() - PERSON_REPOSITORY_CAP
        )));
    }
    column
}

/// The per-user actions: refresh (never automatic) and unfollow.
fn render_person_actions(login: &str, actions: &PersonActions) -> gpui::Div {
    let on_refresh = Rc::clone(&actions.on_refresh);
    let on_unfollow = Rc::clone(&actions.on_unfollow);
    let refresh_target = login.to_string();
    let unfollow_target = login.to_string();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(SharedString::from(format!("people-refresh-{login}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_refresh(&refresh_target, window, cx);
                })
                .child("refresh"),
        )
        .child(
            div()
                .id(SharedString::from(format!("people-unfollow-{login}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_unfollow(&unfollow_target, window, cx);
                })
                .child("unfollow"),
        )
        .child(muted_line("refresh is per user, never automatic"))
}

/// One of the person's repositories: the pitch's triple, clickable —
/// a click opens the repository as a shadow tab, exactly as the
/// DISCOVER results do.
fn render_person_repo(shadow: &ShadowRepo, actions: &PersonActions) -> gpui::AnyElement {
    let on_open = Rc::clone(&actions.on_open_repo);
    let target = shadow.snapshot.clone();
    div()
        .id(SharedString::from(format!(
            "people-repo-{}-{}",
            shadow.snapshot.owner, shadow.snapshot.name
        )))
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .text_size(RowLayout::small_size())
        .text_color(theme::text())
        .on_click(move |_: &ClickEvent, window, cx| on_open(target.clone(), window, cx))
        .child(SharedString::from(triple_line(shadow)))
        .into_any_element()
}

/// The suggestions block: the heading, where the suggestions came from
/// and when, one row per suggestion with its follow action, and the
/// note that says why there are none.
pub(crate) fn render_suggestions(state: &PeopleState, on_follow: &FollowHandler) -> gpui::Div {
    let mut column = div().flex().flex_col().gap_1().px_3();
    column = column.child(
        div()
            .text_size(RowLayout::heading_size())
            .text_color(theme::text_muted())
            .child(SharedString::from("Suggestions")),
    );
    column = column.child(muted_line(&state.suggestion_note()));
    column = column.child(render_suggestion_body(state, on_follow));
    column
}

/// The block's body: one row per suggestion, or the note that says
/// why there are none.
fn render_suggestion_body(state: &PeopleState, on_follow: &FollowHandler) -> gpui::Div {
    if state.suggestions.is_empty() {
        return muted_line("no suggestions yet — type a login to follow someone");
    }
    let rows: Vec<gpui::AnyElement> = state
        .suggestions
        .iter()
        .map(|login| render_suggestion(login, on_follow))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|row| row.into_any_element())
        .collect();
    div().flex().flex_col().gap_1().children(rows)
}

/// One suggestion: the login, and the follow action that follows it.
fn render_suggestion(login: &str, on_follow: &FollowHandler) -> gpui::Div {
    let handler = Rc::clone(on_follow);
    let target = login.to_string();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text())
                .child(SharedString::from(login.to_string())),
        )
        .child(
            div()
                .id(SharedString::from(format!("people-follow-{login}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .on_click(move |_, window, cx| handler(&target, window, cx))
                .child("follow"),
        )
}

/// A quiet line: what the block says when it has nothing to show.
fn muted_line(text: &str) -> gpui::Div {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use yagna_core::model::{Forge, LanguageStat, LicenseInfo, RepoSnapshot};

    fn snapshot(name: &str) -> RepoSnapshot {
        RepoSnapshot {
            forge: Forge::GitHub,
            repo_id: 1,
            owner: "friend".to_string(),
            name: name.to_string(),
            description: None,
            stars: 3,
            languages: vec![LanguageStat {
                name: "Rust".to_string(),
                bytes: 0,
                color: None,
            }],
            license: Some(LicenseInfo {
                name: "MIT".to_string(),
                spdx_id: Some("MIT".to_string()),
            }),
            topics: Vec::new(),
            default_branch: Some("main".to_string()),
            created_at: chrono::Utc
                .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
                .single()
                .expect("date"),
            last_commit_at: chrono::Utc
                .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
                .single()
                .expect("date"),
            starred_at: None,
            commit_count: 0,
            latest_release: None,
            is_fork: false,
            is_archived: false,
        }
    }

    fn shadow(name: &str) -> ShadowRepo {
        ShadowRepo {
            login: "friend".to_string(),
            snapshot: snapshot(name),
            fetched_at: chrono::Utc
                .with_ymd_and_hms(2026, 10, 30, 0, 0, 0)
                .single()
                .expect("date"),
        }
    }

    #[test]
    fn the_triple_line_names_languages_and_stars() {
        let line = triple_line(&shadow("thing"));
        assert!(line.contains("friend/thing"));
        assert!(line.contains("Rust"));
        assert!(line.contains("3 stars"));
    }

    #[test]
    fn refresh_applies_and_unfollow_drops() {
        let mut state = PeopleState {
            refreshing: Some("friend".to_string()),
            ..PeopleState::default()
        };
        state.apply_refresh("friend", vec![shadow("a")]);
        assert_eq!(state.refreshing, None);
        assert!(state.stamps.contains_key("friend"));
        state.drop_login("friend");
        assert!(!state.shadows.contains_key("friend"));
        assert!(!state.stamps.contains_key("friend"));
    }

    /// Two followed people are two drawers, and both keep their data:
    /// following a second person never replaces the first.
    #[test]
    fn two_people_are_two_independent_drawers() {
        let mut state = PeopleState::default();
        state.apply_refresh("alpha", vec![shadow("a")]);
        state.apply_refresh("beta", vec![shadow("b"), shadow("c")]);
        assert_eq!(state.shadows.len(), 2, "both people are held");
        state.toggle_drawer("beta");
        assert_eq!(state.open_drawer.as_deref(), Some("beta"));
        state.toggle_drawer("alpha");
        assert_eq!(
            state.open_drawer.as_deref(),
            Some("alpha"),
            "opening one drawer replaces the open one"
        );
        assert_eq!(state.shadows["beta"].len(), 2, "beta's data survived");
    }

    /// Unfollowing closes and removes that person's drawer, and only
    /// that one's.
    #[test]
    fn unfollowing_removes_only_that_drawer() {
        let mut state = PeopleState::default();
        state.apply_refresh("alpha", vec![shadow("a")]);
        state.apply_refresh("beta", vec![shadow("b")]);
        state.toggle_drawer("beta");
        state.drop_login("beta");
        assert_eq!(state.open_drawer, None, "the drawer is gone");
        assert!(state.shadows.contains_key("alpha"));
    }

    #[test]
    fn the_suggestions_drop_the_followed_and_keep_the_rest_in_order() {
        let logins = ["alpha", "beta", "gamma"]
            .iter()
            .map(|login| login.to_string())
            .collect::<Vec<_>>();
        let followed = vec!["beta".to_string()];
        let suggestions = PeopleState::suggestions_for(&logins, &followed);
        assert_eq!(suggestions, ["alpha".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn a_duplicate_suggestion_collapses_to_one() {
        let logins = ["alpha", "alpha", "beta"]
            .iter()
            .map(|login| login.to_string())
            .collect::<Vec<_>>();
        let suggestions = PeopleState::suggestions_for(&logins, &[]);
        assert_eq!(suggestions, ["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn the_suggestions_are_capped() {
        let logins = (0..20)
            .map(|index| format!("login-{index}"))
            .collect::<Vec<_>>();
        let suggestions = PeopleState::suggestions_for(&logins, &[]);
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS);
        assert_eq!(suggestions.first().map(String::as_str), Some("login-0"));
    }

    #[test]
    fn a_failed_fetch_records_the_reason_and_leaves_no_suggestions() {
        let mut state = PeopleState::default();
        state.apply_suggestions(&["alpha".to_string()], &[]);
        assert_eq!(state.suggestions.len(), 1);
        state.record_suggestion_failure("the network said no");
        assert!(state.suggestions.is_empty());
    }

    /// The suggestion block says where the suggestions came from and
    /// when — and, offline, the last fetch with the reason.
    #[test]
    fn the_suggestion_note_names_the_source_and_the_moment() {
        let mut state = PeopleState::default();
        state.apply_suggestions(&["alpha".to_string()], &[]);
        let landed = state.suggestion_note();
        assert!(
            landed.starts_with("from your GitHub following, fetched "),
            "{landed}"
        );
        assert!(state.suggestion_stamp.is_some());

        state.record_suggestion_failure("the network said no");
        let offline = state.suggestion_note();
        assert!(
            offline.starts_with("offline — the last fetch was "),
            "{offline}"
        );
        assert!(offline.contains("the network said no"), "{offline}");
    }

    #[test]
    fn a_followed_suggestion_leaves_the_block() {
        let mut state = PeopleState::default();
        state.apply_suggestions(&["alpha".to_string(), "beta".to_string()], &[]);
        state.remove_suggestion("alpha");
        assert_eq!(state.suggestions, ["beta".to_string()]);
    }

    /// A stored stamp reads as the reader reads it; a bad one shows as
    /// it was stored.
    #[test]
    fn a_stamp_shows_as_a_moment_and_a_bad_one_as_itself() {
        assert_eq!(
            show_stamp("2026-10-30T00:00:00.000+00:00"),
            "2026-10-30 00:00"
        );
        assert_eq!(show_stamp("not a stamp"), "not a stamp");
    }
}
