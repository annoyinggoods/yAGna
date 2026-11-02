//! The app's face: the window that opens when no verb is given — the
//! starred catalog, its search box, its drawer sidebar, its
//! collapsible rows, and the tabs above them.
//!
//! The UI holds no catalog state of its own: `ListPane` renders the
//! page it fetched; every flip is one store call run on the background
//! executor and crossed back into the entity — the render path never
//! blocks on the database or the network. The child modules own the
//! pieces: rows, search, sidebar (the drawers), tabs/repo (the detail
//! view with its note), readme, tree, settings_panel, usertab,
//! localsurface/localtab (the repositories that live on this machine),
//! notedialog (the note's popup),
//! discover/people (the v2 surfaces), dropdown (the one picker
//! component), shell (the brand chrome and the cards), menu (the
//! in-app menu), workers (the tab's and the row's fetches).
//! Items: `run` — the entry; `ListPane` — the root entity;
//! `page_count_for`/`offset_of` — the page math;
//! `lock_catalog`/`lock_index` — the shared handles' locks.
//! Depends on: yagna_core (sorting, rows, the fuzzy index), yagna_store
//! (the facade), yagna_forge (the runtime), gpui, crate::settings,
//! crate::updater, crate::prefetch.
//! Called by: crate::main.

mod assets;
mod builder;
mod control;
mod discover;
mod dropdown;
mod expand;
mod field;
mod glyphs;
mod icons;
mod localsurface;
mod localtab;
mod menu;
mod notedialog;
mod people;
mod platform;
mod readme;
mod repo;
mod rows;
mod scoresurface;
mod search;
mod services;
mod settings_panel;
mod shell;
mod sidebar;
mod tabs;
mod theme;
mod tooltip;
mod tree;
mod typography;
pub(crate) mod updatestate;
mod usertab;
mod workers;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, ClipboardItem, Context, FocusHandle,
    KeyDownEvent, ListAlignment, ListState, Pixels, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, list, prelude::*, px, size,
};
use yagna_core::fuzzy::{FuzzyIndex, SearchHit as FuzzyHit, SearchItem, haystack};
use yagna_core::model::{
    DiscoverQuery, LocalRepo, NoteSubject, RepoRow, RepoSnapshot, ShadowRepo, SortKey,
};
use yagna_core::query;
use yagna_forge::{Forge, SEARCH_DEBOUNCE_SECS};
use yagna_store::{
    CatalogFilter, EXPANDED_ROWS_KEY, FIRST_COMMIT_SOURCE_DEFAULT, ImagePolicy, PrefetchPlan,
    PrefetchScope, PruneReport, SIDEBAR_DRAWER_KEY, SearchHit, SearchRow, SearchSource, Store,
    StoreError, TagKind, topic_counts,
};

use crate::settings::Settings;
use crate::updater::{UpdateMessage, UpdateWorker};

use dropdown::{DropdownActions, DropdownOption};
use expand::{ExpandedState, RowPreview};
use field::{FieldAction, FieldKind, TextField};
use glyphs::BuiltinAssets;
use localsurface::LocalActions as LocalSurfaceActions;
use localtab::LocalActions as LocalTabActions;
use notedialog::NoteActions;
use repo::TabViewActions;
use rows::{Chevron, RowActions, RowDisplay, RowLayout, build_display, shape_excerpt};
use search::SearchState;
use services::Services;
use settings_panel::{SettingsAction, SettingsActions};
use shell::{Dialog, Notification};
use sidebar::{DrawerKind, EditorTags, SidebarAction, SidebarState};
use tabs::{TabData, TabState};

/// How long the launch auto-update waits after first frame.
const AUTO_UPDATE_DELAY: Duration = Duration::from_secs(5);
/// How often the UI polls the update worker's channel.
const UPDATE_POLL: Duration = Duration::from_millis(120);
/// How often the update card repaints while a sweep runs — the clock
/// that moves on its own, one card, once a second.
const UPDATE_CLOCK: Duration = Duration::from_secs(1);
/// How many top rows the user tab's aggregate sections show.
const STATS_LIMIT: u32 = 10;

/// The window's default size.
const WINDOW_SIZE: (f32, f32) = (1200.0, 800.0);
/// The README lines an open row shows.
pub(crate) const PREVIEW_LINES: usize = 50;
/// The sidebar's width.
const SIDEBAR_WIDTH: Pixels = px(230.0);
/// How many image workers run at once — the queue's pace, not the tab's.
const IMAGE_CONCURRENCY: usize = 4;
/// The search field's width: wide enough for a repository's name, and
/// the same width whatever is typed into it.
const SEARCH_WIDTH: Pixels = px(560.0);
/// The catalog's width: the rows sit in a column of their own, so a
/// wide window does not stretch a repository across the screen.
const LIST_WIDTH: Pixels = px(1180.0);
/// How many hits one deep search serves.
const DEEP_LIMIT: usize = 50;
/// The deep-search panel's results height: a scroll of its own, so the
/// catalog stays where it is.
const DEEP_RESULTS_HEIGHT: Pixels = px(320.0);

/// The tab order: the search comes first, the deep query next, the
/// discovery filters after it, the follow field after that, and the jump
/// field last — the reader moves through the window's controls in the
/// order the window reads.
const TAB_SEARCH: isize = 0;
/// The deep search's own field.
const TAB_DEEP: isize = 1;
/// The discovery filters: the days, the stars, the language, the
/// topic, the word.
const TAB_DISCOVER: isize = 2;
/// The follow field.
const TAB_FOLLOW: isize = 7;
/// The jump-to-page field.
const TAB_JUMP: isize = 8;
/// The chrome rows' height at the ramp's own size: the title bar, the
/// tab strip and the library header. One measure for the three of
/// them, so the sort menu can find the library header's bottom edge.
const CHROME_HEIGHT: f32 = 48.0;
/// The DISCOVER results' height: a scroll of its own, so the controls
/// above stay where they are.
const DISCOVER_RESULTS_HEIGHT: Pixels = px(420.0);
/// The step the menu's text-size commands move the type ramp by.
const FONT_SCALE_STEP: i32 = 10;
/// The issue tracker the menu's "Report an issue" opens.
const ISSUE_TRACKER_URL: &str = "https://github.com/mrmflis-hub/yAGna/issues";
/// The name the export verb gives its files, and the extension it
/// uses. The verb's own helper is private to its module, so the
/// pattern is repeated here for the import that has to find what the
/// export wrote — the two must change together.
const EXPORT_PREFIX: &str = "yagna-export-";
/// The export file's extension.
const EXPORT_SUFFIX: &str = ".json";

/// The last page with content for a total count (at least one, so the
/// footer reads a page even while the catalog is empty).
fn page_count_for(total: u64, page_size: u32) -> u32 {
    (total.div_ceil(u64::from(page_size)) as u32).max(1)
}

/// The row offset a 1-based page starts at.
fn offset_of(page: u32, page_size: u32) -> u32 {
    (page.max(1) - 1).saturating_mul(page_size)
}

/// Whether a verb's exit code says it succeeded.
fn verb_succeeded(code: std::process::ExitCode) -> bool {
    code == std::process::ExitCode::SUCCESS
}

/// One chrome row's height, through the reader's type scale: the
/// title bar, the tab strip and the library header are all this tall.
fn chrome_height() -> Pixels {
    px(CHROME_HEIGHT * typography::scale())
}

/// The next key in the sort cycle the settings surface walks.
fn next_sort_key(current: SortKey) -> SortKey {
    let all = SortKey::ALL;
    let index = all.iter().position(|key| *key == current).unwrap_or(0);
    all[(index + 1) % all.len()]
}

/// The theme after the current one: the embedded themes in picker
/// order, wrapping round. A name the picker does not know starts the
/// cycle at the first theme.
fn next_theme_name(current: &str) -> &'static str {
    let names = theme::THEME_NAMES;
    let index = names.iter().position(|name| *name == current).unwrap_or(0);
    names[(index + 1) % names.len()]
}

/// The export file the import reads: the newest one in the working
/// directory, which is where the export verb writes. `None` when the
/// directory holds no export at all.
fn newest_export() -> Option<PathBuf> {
    let entries = std::fs::read_dir(".").ok()?;
    let candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with(EXPORT_PREFIX) || !name.ends_with(EXPORT_SUFFIX) {
                return None;
            }
            let modified = path.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    candidates
        .into_iter()
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Opens the catalog and the window. A catalog that cannot open (or a
/// window that cannot open) is a printed error and a failure code — the
/// app reports, never repairs. With `mark_startup`, the launch path is
/// measured and the report printed before the app quits: the
/// cold-start gate's instrumented run.
pub(crate) fn run(mark_startup: bool) -> std::process::ExitCode {
    let startup = crate::startup::Startup::new();
    let catalog = match crate::paths::open_catalog() {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    startup.mark_since_start(crate::startup::Slot::Store, Instant::now());
    let cache = crate::paths::cache().unwrap_or_else(|_| PathBuf::from("."));
    let token = crate::token::acquire(false);
    let services = Arc::new(services::Services::open(token.as_deref()));
    // The settings and the theme come before the window: the first
    // frame already paints the palette the settings name.
    let (settings, settings_note) = crate::settings::load();
    // The reader's type scale too: the first frame is already painted
    // at the size the file names.
    typography::set_scale(settings.font_scale);
    let data_dir = crate::paths::data_dir().ok();
    let theme_note = match theme::load_active(&settings.theme, data_dir.as_deref()) {
        Ok(palette) => theme::apply(&palette).err(),
        Err(problem) => Some(problem),
    };
    let notice = settings_note.or(theme_note);
    // One session instant: age labels are stable for the session, and
    // the pure core never reads a clock.
    let now = Utc::now();
    let window_failed = Rc::new(Cell::new(false));
    let on_open_failure = Rc::clone(&window_failed);
    Application::new()
        .with_assets(BuiltinAssets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)), cx);
            let pane = cx.new(|cx| {
                ListPane::new(
                    PaneSetup {
                        catalog: Arc::clone(&catalog),
                        services: services.clone(),
                        cache_dir: cache.clone(),
                        settings,
                        notice,
                        now,
                        startup: Arc::clone(&startup),
                        mark_startup,
                    },
                    cx,
                )
            });
            let list_focus = pane.read(cx).list_focus.clone();
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("yAGna".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |_, _| pane.clone(),
            );
            match opened {
                Ok(window) => {
                    let _ = window.update(cx, |_, window, _| window.focus(&list_focus));
                    cx.activate(true);
                    if mark_startup {
                        report_startup(Arc::clone(&startup), cx);
                    }
                }
                Err(error) => {
                    eprintln!("the yAGna window cannot open: {error}");
                    on_open_failure.set(true);
                    cx.quit();
                }
            }
        });
    if window_failed.get() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// The instrumented run's closing move: wait for the launch path's
/// marks (the first frame and the first page query), print the
/// report, and leave. The run exists to measure, not to be read — and
/// GPUI's graceful quit lives on the application handle, which a
/// background task cannot reach, so the process ends here instead.
fn report_startup(startup: Arc<crate::startup::Startup>, cx: &mut App) {
    cx.background_spawn(async move {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while Instant::now() < deadline {
            let marks = startup.marks();
            if marks.frame.is_some() && marks.page.is_some() && marks.index.is_some() {
                break;
            }
            // A background thread's sleep: the app crate holds no
            // async timer of its own, and this task exists only in the
            // instrumented run.
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        startup.report();
        std::process::exit(0);
    })
    .detach();
}

/// The list surface: what the window renders. The page it holds is the
/// result of the last fetch; the total count and the sort key drive the
/// header and the footer.
struct ListPane {
    catalog: Arc<Mutex<Store>>,
    /// The network side: the runtime and the two forges.
    services: Arc<Services>,
    /// The open tabs and the focused one.
    tabs: TabState,
    /// The settings one session runs with.
    settings: Settings,
    cache_dir: PathBuf,
    rows: Vec<RepoRow>,
    total: u64,
    /// The visible page, 1-based.
    page: u32,
    sort: SortKey,
    loading: bool,
    error: Option<String>,
    /// The last thing the tab did, said in the status slot.
    notice: Option<String>,
    /// The one dropdown the pane holds open — never two at once.
    dropdown: Option<dropdown::Site>,
    /// The option the keyboard is on, in the open dropdown's list.
    dropdown_highlight: usize,
    /// The menu head whose dropdown is open, when one is.
    menu_head: Option<menu::Head>,
    /// The window's tooltip: the name a hovered control is announced
    /// with.
    tooltip: tooltip::TooltipHandle,
    /// The card the shell has open over the window, when one is.
    dialog: Option<Dialog>,
    /// The cards waiting in the notification slot, oldest first.
    notifications: Vec<Notification>,
    /// Whether the tag sidebar shows.
    sidebar_open: bool,
    /// The session instant the age labels are measured against.
    now: DateTime<Utc>,
    /// The per-row view models of the current page; `None` until first
    /// painted, then cached for the scroll path.
    displays: Vec<Option<RowDisplay>>,
    /// Shaped excerpts keyed by (row index, content width in whole
    /// pixels) — measured once per row per width, reused every frame.
    excerpts: HashMap<(usize, u32), rows::ExcerptLines>,
    /// The list's own state: the measured heights of the rows.
    list_state: ListState,
    /// The list's focus handle: the keyboard map hangs off it.
    list_focus: FocusHandle,
    /// Which row the arrow keys walked to.
    focused_row: usize,
    /// The expansion state: the open rows and their previews.
    expanded: ExpandedState,
    /// The search box: the query and the debounce generation.
    search: SearchState,
    /// The search field's element state.
    search_field: TextField,
    /// The sidebar: the tag filter and the tag editor.
    sidebar: SidebarState,
    /// The tag editor's new-tag field.
    tag_field: TextField,
    /// The rename field: the tag's name, edited in place.
    rename_field: TextField,
    /// The sidebar's filter box: one word that narrows the tags, the
    /// topics and the notes at once.
    sidebar_filter_field: TextField,
    /// The note's editor: the paragraph the reader writes in. One
    /// editor at a time — `note_editing` names whose it is.
    note_field: TextField,
    /// The subject whose note is being edited, when one is: a catalog
    /// row or a local entry.
    note_editing: Option<NoteSubject>,
    /// The local repositories the last scan found.
    local_repos: Vec<LocalRepo>,
    /// Whether the folder is being scanned right now.
    local_scanning: bool,
    /// The score surface's loaded facts — one read per open.
    score_view: Option<scoresurface::ScoreView>,
    /// The spec the score surface last showed: what a reload's change
    /// line compares against.
    score_previous: Option<yagna_core::fit::FitSpec>,
    /// Whether the catalog is being re-scored right now.
    score_busy: bool,
    /// What the last re-score reported, until the next one.
    score_note: Option<String>,
    /// The score surface's spec editor: the file's JSON, applied and
    /// reverted in place — the reader's own hand on the fit score.
    spec_field: TextField,
    /// The text the spec field was filled from; a reader's typed edit
    /// — any divergence — survives a reload untouched.
    spec_field_loaded: String,
    /// The agent presence line for the status row — from the trace's
    /// newest heartbeat, refreshed on a timer.
    agent_line: Option<String>,
    /// The local folder field of the settings modal.
    local_folder_field: TextField,
    /// The library sort dropdown's trigger focus.
    sort_focus: FocusHandle,
    /// The DISCOVER sort dropdown's trigger focus.
    discover_sort_focus: FocusHandle,
    /// The builder's field picker trigger focus.
    builder_field_pick_focus: FocusHandle,
    /// The builder's operator picker trigger focus.
    builder_op_pick_focus: FocusHandle,
    /// The jump-to-page field.
    jump_field: TextField,
    /// The fuzzy index over the catalog — a memory-only projection.
    index: Arc<Mutex<FuzzyIndex>>,
    /// What the index build measured, for the status line.
    index_note: String,
    /// Whether the index is ready to answer.
    index_ready: bool,
    /// Whether the deep-search panel is open.
    deep_open: bool,
    /// The deep-search field: the query grammar's text.
    deep_field: TextField,
    /// The builder's state: the clauses the deep field holds, the
    /// draft being composed, and the last refusal.
    builder: builder::BuilderState,
    /// The builder's own value field.
    builder_field: TextField,
    /// The deep search's hits, newest query's.
    deep_hits: Vec<SearchHit>,
    /// The query that made the current hits — a revised field re-runs
    /// instead of opening the stale selection.
    deep_query: String,
    /// Which deep hit the keyboard is on.
    deep_selected: usize,
    /// What the last deep search said: the parse error, the empty
    /// result, or nothing.
    deep_note: Option<String>,
    /// Whether a deep search is in flight.
    deep_running: bool,
    /// Whether the settings modal is open.
    settings_open: bool,
    /// The cache-cap field of the settings modal.
    cache_field: TextField,
    /// The page-size field of the settings modal.
    page_field: TextField,
    /// The update worker: one sweep at a time.
    updater: UpdateWorker,
    /// The worker's channel, polled from the UI.
    update_rx: Arc<Mutex<Receiver<UpdateMessage>>>,
    /// The user tab's aggregates, fetched when the tab opens.
    user_data: Option<usertab::UserStatsData>,
    /// Whether the user tab's aggregates are on their way.
    user_loading: bool,
    /// The rows whose missing README has been asked for on demand —
    /// one fetch per row, never a burst.
    readme_asked_rows: HashSet<i64>,
    /// The rows whose README is being fetched on demand right now.
    readme_fetching_rows: HashSet<i64>,
    /// Why a row's on-demand README fetch failed, when it did — the
    /// one line the open row shows instead of nothing.
    readme_failed_rows: HashMap<i64, String>,
    /// The DISCOVER tab's query, results and budget.
    discover: discover::DiscoverState,
    /// The DISCOVER controls: days, stars, language, topic, word.
    discover_days_field: TextField,
    discover_stars_field: TextField,
    discover_language_field: TextField,
    discover_topic_field: TextField,
    discover_word_field: TextField,
    /// The PEOPLE tab's shadows and stamps per login.
    people: people::PeopleState,
    /// The follow field: one login, enter follows.
    follow_field: TextField,
    /// The next shadow tab's row id: negative, decreasing — catalog
    /// row ids are positive, so a shadow tab never collides.
    shadow_next: i64,
    /// The data directory, for the persisted expansion set and the
    /// theme overrides.
    data_dir: Option<PathBuf>,
    /// The launch path's marks, for the cold-start gate's
    /// instrumented run.
    startup: Arc<crate::startup::Startup>,
    /// Whether this session is the instrumented run: it prints the
    /// startup report and quits.
    mark_startup: bool,
    /// Whether the first frame has been marked.
    frame_marked: bool,
}

/// Everything the score surface's fetch reads, before the pane shapes
/// it into the view.
struct ScoreStats {
    spec: yagna_core::fit::FitSpec,
    loud_line: Option<String>,
    source: String,
    spec_changed: Option<String>,
    catalogue: Vec<(String, String)>,
    health: yagna_store::HealthBuckets,
    fit: yagna_store::FitSpread,
    rescored_at: Option<String>,
    rescored_rows: u64,
    rescored_moved: u64,
}

/// What the pane is built from: the catalog, the services, the paths,
/// the settings, the session's notice and clock, and the launch probe.
/// One struct, so the constructor stays a constructor.
struct PaneSetup {
    catalog: Arc<Mutex<Store>>,
    services: Arc<Services>,
    cache_dir: PathBuf,
    settings: Settings,
    notice: Option<String>,
    now: DateTime<Utc>,
    startup: Arc<crate::startup::Startup>,
    mark_startup: bool,
}

impl ListPane {
    fn new(setup: PaneSetup, cx: &mut Context<Self>) -> Self {
        let PaneSetup {
            catalog,
            services,
            cache_dir,
            settings,
            notice,
            now,
            startup,
            mark_startup,
        } = setup;
        let list_focus = cx.focus_handle();
        let search_focus = cx.focus_handle();
        let jump_focus = cx.focus_handle();
        let tag_focus = cx.focus_handle();
        let rename_focus = cx.focus_handle();
        let sidebar_filter_focus = cx.focus_handle();
        let note_focus = cx.focus_handle();
        let local_folder_focus = cx.focus_handle();
        let deep_focus = cx.focus_handle();
        let builder_focus = cx.focus_handle();
        let cache_focus = cx.focus_handle();
        let page_focus = cx.focus_handle();
        let discover_days_focus = cx.focus_handle();
        let discover_stars_focus = cx.focus_handle();
        let discover_language_focus = cx.focus_handle();
        let discover_topic_focus = cx.focus_handle();
        let discover_word_focus = cx.focus_handle();
        let follow_focus = cx.focus_handle();
        let spec_focus = cx.focus_handle();
        let sort_pick_focus = cx.focus_handle();
        let discover_sort_pick_focus = cx.focus_handle();
        let builder_field_pick_focus = cx.focus_handle();
        let builder_op_pick_focus = cx.focus_handle();
        let (dummy_sender, update_rx) = std::sync::mpsc::channel();
        drop(dummy_sender);
        let data_dir = crate::paths::data_dir().ok();
        let initial_sort = settings.sort_default;
        let mut pane = Self {
            catalog,
            services,
            tabs: TabState::new(),
            settings,
            cache_dir,
            rows: Vec::new(),
            total: 0,
            page: 1,
            sort: initial_sort,
            loading: true,
            error: None,
            notice,
            dropdown: None,
            dropdown_highlight: 0,
            menu_head: None,
            tooltip: tooltip::TooltipHandle::install(cx),
            dialog: None,
            notifications: Vec::new(),
            sidebar_open: true,
            now,
            displays: Vec::new(),
            excerpts: HashMap::new(),
            // `measure_all` measures every row on the first layout: the
            // scrollbar and the scroll mapping then describe the whole
            // page, not only the rows that happened to be on screen.
            list_state: ListState::new(0, ListAlignment::Top, px(0.0)).measure_all(),
            list_focus,
            focused_row: 0,
            expanded: ExpandedState::default(),
            search: SearchState::default(),
            search_field: TextField::new(
                "search",
                search_focus,
                "search the catalog…  ( / )",
                FieldKind::Text,
            )
            .at_tab_index(TAB_SEARCH),
            sidebar: SidebarState::new(),
            tag_field: TextField::new("tag", tag_focus, "new tag…", FieldKind::Text),
            rename_field: TextField::new("rename", rename_focus, "new name…", FieldKind::Text),
            sidebar_filter_field: TextField::new(
                "sidebar-filter",
                sidebar_filter_focus,
                "filter tags, topics, notes…",
                FieldKind::Text,
            ),
            note_field: TextField::new("note", note_focus, "write a note…", FieldKind::Paragraph),
            note_editing: None,
            local_repos: Vec::new(),
            local_scanning: false,
            local_folder_field: TextField::new(
                "local-folder",
                local_folder_focus,
                "the folder of clones…",
                FieldKind::Text,
            ),
            sort_focus: sort_pick_focus,
            discover_sort_focus: discover_sort_pick_focus,
            builder_field_pick_focus,
            builder_op_pick_focus,
            jump_field: TextField::new("jump", jump_focus, "go to page…", FieldKind::Digits)
                .at_tab_index(TAB_JUMP),
            index: Arc::new(Mutex::new(FuzzyIndex::default())),
            index_note: String::new(),
            index_ready: false,
            score_view: None,
            score_previous: None,
            score_busy: false,
            score_note: None,
            spec_field: TextField::new(
                "spec",
                spec_focus,
                "the spec as JSON…",
                FieldKind::Paragraph,
            )
            .with_measure_family(theme::MONO_FONT),
            spec_field_loaded: String::new(),
            agent_line: None,
            deep_open: false,
            deep_field: TextField::new(
                "deep",
                deep_focus,
                "deep search…  (reading lang:rust stars:>1000)",
                FieldKind::Text,
            )
            .at_tab_index(TAB_DEEP),
            builder: builder::BuilderState::new(),
            builder_field: TextField::new("builder", builder_focus, "value…", FieldKind::Text)
                .at_tab_index(TAB_DEEP + 1),
            deep_hits: Vec::new(),
            deep_query: String::new(),
            deep_selected: 0,
            deep_note: None,
            deep_running: false,
            settings_open: false,
            cache_field: TextField::new("cache", cache_focus, "megabytes…", FieldKind::Digits),
            page_field: TextField::new("page-size", page_focus, "rows…", FieldKind::Digits),
            updater: UpdateWorker::default(),
            update_rx: Arc::new(Mutex::new(update_rx)),
            user_data: None,
            user_loading: false,
            readme_asked_rows: HashSet::new(),
            readme_fetching_rows: HashSet::new(),
            readme_failed_rows: HashMap::new(),
            discover: discover::DiscoverState::default(),
            discover_days_field: TextField::new(
                "discover-days",
                discover_days_focus,
                "days…",
                FieldKind::Digits,
            )
            .at_tab_index(TAB_DISCOVER),
            discover_stars_field: TextField::new(
                "discover-stars",
                discover_stars_focus,
                "stars…",
                FieldKind::Digits,
            )
            .at_tab_index(TAB_DISCOVER + 1),
            discover_language_field: TextField::new(
                "discover-language",
                discover_language_focus,
                "language…",
                FieldKind::Text,
            )
            .at_tab_index(TAB_DISCOVER + 2),
            discover_topic_field: TextField::new(
                "discover-topic",
                discover_topic_focus,
                "one topic…",
                FieldKind::Text,
            )
            .at_tab_index(TAB_DISCOVER + 3),
            discover_word_field: TextField::new(
                "discover-word",
                discover_word_focus,
                "a word…",
                FieldKind::Text,
            )
            .at_tab_index(TAB_DISCOVER + 4),
            people: people::PeopleState::default(),
            follow_field: TextField::new(
                "follow",
                follow_focus,
                "follow a login…",
                FieldKind::Text,
            )
            .at_tab_index(TAB_FOLLOW),
            shadow_next: -1,
            data_dir,
            startup,
            mark_startup,
            frame_marked: false,
        };
        pane.restore_expansion(cx);
        pane.restore_sidebar_drawer(cx);
        pane.poll_update_channel(cx);
        pane.maybe_auto_update_on_launch(cx);
        if !pane.mark_startup {
            // The instrumented run measures the launch path: the
            // cache's prune would work in the background and blur the
            // numbers.
            pane.prune_cache(cx);
        }
        pane.request_page(cx);
        pane.build_index(cx);
        pane.refresh_sidebar(cx);
        pane.load_discover_cache(cx);
        pane.load_people_cache(cx);
        if pane.settings.local_folder.is_some() {
            // The folder is named: the worktrees under it are found
            // once at launch, and the reader may scan again.
            pane.scan_local(cx);
        }
        pane.watch_agent_presence(cx);
        pane
    }

    /// The last page with content, given the current total.
    fn page_count(&self) -> u32 {
        page_count_for(self.total, self.page_size())
    }

    /// The store offset the current page starts at.
    fn offset(&self) -> u32 {
        offset_of(self.page, self.page_size())
    }

    /// Rows per page, as the settings hold it (a session keeps the
    /// value it launched with until the settings surface commits).
    fn page_size(&self) -> u32 {
        self.settings.page_size
    }

    /// Whether a query is typed — the session is a ranked one.
    fn is_searching(&self) -> bool {
        self.search.is_active()
    }

    // --- The persisted expansion.

    /// Restores the persisted expansion set, when the setting allows
    /// it: one read, the open rows come back.
    fn restore_expansion(&mut self, cx: &mut Context<Self>) {
        if !self.settings.persist_expansion {
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.app_state(EXPANDED_ROWS_KEY)
        });
        cx.spawn(async move |this, cx| {
            let stored = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_restored_expansion(stored, cx));
        })
        .detach();
    }

    /// Lands the restored set.
    fn apply_restored_expansion(
        &mut self,
        stored: Result<Option<String>, StoreError>,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.persist_expansion {
            return;
        }
        if let Ok(Some(json)) = stored
            && let Ok(ids) = serde_json::from_str::<Vec<i64>>(&json)
        {
            self.expanded.restore(&ids);
        }
        cx.notify();
    }

    /// Saves the open set to the store, when the setting says so.
    fn persist_expansion(&self, cx: &Context<Self>) {
        if !self.settings.persist_expansion {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.expanded.open_ids()) else {
            return;
        };
        let catalog = Arc::clone(&self.catalog);
        let write = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.set_app_state(EXPANDED_ROWS_KEY, &json)
        });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            if let Err(problem) = written {
                let _ = this.update(cx, |pane, _cx| pane.notice = Some(problem.to_string()));
            }
        })
        .detach();
    }

    // --- The persisted sidebar drawer.

    /// Restores the persisted open drawer: one read, the drawer the
    /// reader left open comes back.
    fn restore_sidebar_drawer(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.app_state(SIDEBAR_DRAWER_KEY)
        });
        cx.spawn(async move |this, cx| {
            let stored = fetch.await;
            let _ = this.update(cx, |pane, cx| {
                if let Ok(Some(name)) = stored {
                    pane.sidebar.restore_drawer(Some(&name));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the open drawer to the store — the drawer's own key,
    /// beside the expansion set's.
    fn persist_drawer(&self, cx: &Context<Self>) {
        let name = self.sidebar.open_drawer().map(|drawer| drawer.name());
        let catalog = Arc::clone(&self.catalog);
        let write = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            match name {
                Some(name) => store.set_app_state(SIDEBAR_DRAWER_KEY, name),
                None => store.set_app_state(SIDEBAR_DRAWER_KEY, ""),
            }
        });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            if let Err(problem) = written {
                let _ = this.update(cx, |pane, _cx| pane.notice = Some(problem.to_string()));
            }
        })
        .detach();
    }

    // --- The update worker.

    /// Starts polling the update worker's channel; the loop runs for
    /// the session, one cheap `try_recv` per tick.
    fn poll_update_channel(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(UPDATE_POLL).await;
                let alive = this.update(cx, |pane, cx| {
                    let messages = pane.drain_update_messages();
                    pane.apply_update_messages(messages, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Takes everything the worker has said so far.
    fn drain_update_messages(&self) -> Vec<UpdateMessage> {
        let receiver = self
            .update_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        messages
    }

    /// Lands whatever the worker said: progress lines go to the status
    /// slot, the summary refetches the page and the tags.
    fn apply_update_messages(&mut self, messages: Vec<UpdateMessage>, cx: &mut Context<Self>) {
        if messages.is_empty() {
            return;
        }
        for message in messages {
            match message {
                UpdateMessage::Progress(line) => {
                    // The worker's own status carries the line the
                    // sweep is on — the status line shows it.
                    let progress = self
                        .updater
                        .status()
                        .progress()
                        .map(str::to_string)
                        .unwrap_or(line);
                    self.notice = Some(progress);
                }
                UpdateMessage::Finished(outcome) => {
                    // The sweep is over: the status line's transient
                    // progress goes, and the finished state lands in
                    // its own card.
                    self.notice = None;
                    self.announce_update(outcome);
                    self.request_page(cx);
                    self.refresh_sidebar(cx);
                    self.build_index(cx);
                    if self.tabs.surface_in_front(tabs::Surface::User) {
                        self.load_user_stats(cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// The finished sweep's card: the summary as a fact, the problem as
    /// an alarm. The card waits in the notification slot until the
    /// reader dismisses it.
    fn announce_update(&mut self, outcome: Result<String, String>) {
        let notification = match outcome {
            Ok(summary) => Notification {
                verb: "Update finished".to_string(),
                body: summary,
                error: false,
            },
            Err(problem) => Notification {
                verb: "Update failed".to_string(),
                body: problem,
                error: true,
            },
        };
        self.notifications.push(notification);
    }

    /// UPDATE REPOSITORIES: the sweep on the worker thread, one at a
    /// time. A second click while one runs is answered with the age of
    /// the run already on, never a second thread.
    fn start_update(&mut self, cx: &mut Context<Self>) {
        if self.updater.is_running() {
            let now = Instant::now();
            self.notice = self
                .updater
                .status()
                .already_running_line(now)
                .or_else(|| Some("update already running".to_string()));
            cx.notify();
            return;
        }
        let Some(token) = self.services.token().map(|token| token.to_string()) else {
            self.notice = Some("no token — set GITHUB_TOKEN to update".to_string());
            cx.notify();
            return;
        };
        let sender = {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut rx = self
                .update_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // A fresh channel per run: the poll loop reads whichever
            // receiver the pane currently holds.
            *rx = receiver;
            sender
        };
        if self.updater.try_start(token, sender) {
            self.notice = Some("updating repositories…".to_string());
            self.start_update_clock(cx);
        } else {
            self.notice = Some("update already running".to_string());
        }
        cx.notify();
    }

    /// The update's clock: once a second, while a sweep runs, the pane
    /// repaints — the card's elapsed time moves on its own, without
    /// the reader touching the mouse. The loop ends with the run; the
    /// caret's blink is its own animation and never runs here.
    fn start_update_clock(&mut self, cx: &mut Context<Self>) {
        if !self.updater.is_running() {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(UPDATE_CLOCK).await;
                let still_running = this.update(cx, |pane, cx| {
                    let on = pane.updater.status().is_running();
                    if on {
                        cx.notify();
                    }
                    on
                });
                if still_running.is_err() || !still_running.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Stops the running sweep: the worker is asked to stop at the next
    /// page boundary, and the card says "stopping…" until it lands.
    fn stop_update(&mut self, cx: &mut Context<Self>) {
        if !self.updater.status().stoppable() {
            return;
        }
        self.updater.request_stop();
        self.notice = None;
        cx.notify();
    }

    /// The launch hook: when the setting says so, the sweep fires a few
    /// seconds after first frame — silently, failures in the status
    /// line only.
    fn maybe_auto_update_on_launch(&mut self, cx: &mut Context<Self>) {
        if !self.settings.auto_update_on_launch {
            return;
        }
        if self.services.token().is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_UPDATE_DELAY).await;
            let _ = this.update(cx, |pane, cx| pane.start_update(cx));
        })
        .detach();
    }

    // --- The user tab.

    /// Fetches the user tab's aggregates: the overview and the four
    /// stats queries, one lock, one background task.
    fn load_user_stats(&mut self, cx: &mut Context<Self>) {
        self.user_loading = true;
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let overview = store.overview()?;
            let languages = store.stats_by_language(STATS_LIMIT)?;
            let health = store.stats_health_buckets()?;
            let years = store.stats_per_year()?;
            let tags = store.stats_top_tags(STATS_LIMIT)?;
            let topics = store.stats_top_topics(STATS_LIMIT)?;
            Ok::<_, StoreError>(usertab::UserStatsData {
                overview,
                languages,
                health,
                years,
                tags,
                topics,
            })
        });
        cx.spawn(async move |this, cx| {
            let data = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_user_stats(data, cx));
        })
        .detach();
    }

    /// Lands the user tab's aggregates.
    fn apply_user_stats(
        &mut self,
        data: Result<usertab::UserStatsData, StoreError>,
        cx: &mut Context<Self>,
    ) {
        self.user_loading = false;
        match data {
            Ok(data) => self.user_data = Some(data),
            Err(problem) => self.notice = Some(problem.to_string()),
        }
        cx.notify();
    }

    // --- The DISCOVER tab: one query shape over the search API.

    /// Primes the DISCOVER controls from the query the surface holds.
    fn prime_discover_controls(&mut self) {
        self.discover_days_field
            .set(&self.discover.query.created_within_days.to_string());
        self.discover_stars_field
            .set(&self.discover.query.min_stars.to_string());
        self.discover_language_field.set(
            self.discover
                .query
                .language
                .clone()
                .unwrap_or_default()
                .as_str(),
        );
        self.discover_topic_field.set(
            self.discover
                .query
                .topic
                .clone()
                .unwrap_or_default()
                .as_str(),
        );
        self.discover_word_field.set(
            self.discover
                .query
                .word
                .clone()
                .unwrap_or_default()
                .as_str(),
        );
    }

    /// Reads the stored DISCOVER shadows: offline shows the last fetch.
    fn load_discover_cache(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.shadows_for(yagna_store::DISCOVER_LOGIN)
        });
        cx.spawn(async move |this, cx| {
            let cached = fetch.await;
            let _ = this.update(cx, |pane, cx| {
                if let Ok(shadows) = cached {
                    pane.discover.results =
                        shadows.into_iter().map(|shadow| shadow.snapshot).collect();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Commits the DISCOVER controls into the query: bad values keep
    /// what the query held.
    fn commit_discover_query(&mut self) {
        let days = self.discover_days_field.value().trim().to_string();
        let stars = self.discover_stars_field.value().trim().to_string();
        let language = self.discover_language_field.value().trim().to_string();
        let topic = self.discover_topic_field.value().trim().to_string();
        let word = self.discover_word_field.value().trim().to_string();
        self.discover_days_field.clear();
        self.discover_stars_field.clear();
        self.discover_language_field.clear();
        self.discover_topic_field.clear();
        self.discover_word_field.clear();
        if let Ok(days) = days.parse::<u32>()
            && days > 0
            && days <= 365
        {
            self.discover.query.created_within_days = days;
        }
        if let Ok(stars) = stars.parse::<u32>() {
            self.discover.query.min_stars = stars;
        }
        if language.is_empty() {
            self.discover.query.language = None;
        } else {
            self.discover.query.language = Some(language);
        }
        if topic.is_empty() {
            self.discover.query.topic = None;
        } else {
            self.discover.query.topic = Some(topic);
        }
        if word.is_empty() {
            self.discover.query.word = None;
        } else {
            self.discover.query.word = Some(word);
        }
    }

    /// Puts the DISCOVER filters back the way they came: the default
    /// query, no results, no error.
    fn clear_discover_filters(&mut self, cx: &mut Context<Self>) {
        self.discover.clear_filters();
        self.discover_days_field.clear();
        self.discover_stars_field.clear();
        self.discover_language_field.clear();
        self.discover_topic_field.clear();
        self.discover_word_field.clear();
        cx.notify();
    }

    /// Runs the DISCOVER query: one query per interaction, debounced at
    /// the search budget's floor. The results land as shadow rows under
    /// the search login, so offline shows the last fetch.
    fn run_discover(&mut self, cx: &mut Context<Self>) {
        if !self.discover.can_run() {
            self.discover.note = Some(format!(
                "one query per interaction — wait {SEARCH_DEBOUNCE_SECS}s between searches"
            ));
            cx.notify();
            return;
        }
        self.commit_discover_query();
        if self.services.api.is_none() {
            self.discover.note = Some("no token — set GITHUB_TOKEN to search".to_string());
            cx.notify();
            return;
        }
        let services = Arc::clone(&self.services);
        let query = self.discover.query.clone();
        let now = self.now;
        self.discover.running = true;
        self.discover.last_run = Some(Instant::now());
        self.discover.note = None;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let (api, host) = match (services.api.as_ref(), services.host.as_ref()) {
                (Some(api), Some(host)) => (api, host),
                _ => {
                    return Err(yagna_forge::ForgeError::Parse(
                        "no token or runtime — cannot search".to_string(),
                    ));
                }
            };
            host.search(api, &query, now)
                .map(|snapshots| (query, snapshots))
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_discover(result, catalog, cx));
        })
        .detach();
    }

    /// Lands the DISCOVER results: stored as shadows, counted against
    /// the probe's search budget. A failed run leaves the last results
    /// standing and says what went wrong.
    fn apply_discover(
        &mut self,
        result: Result<(DiscoverQuery, Vec<RepoSnapshot>), yagna_forge::ForgeError>,
        catalog: Arc<Mutex<Store>>,
        cx: &mut Context<Self>,
    ) {
        self.discover.running = false;
        let (query, snapshots) = match result {
            Ok(landed) => landed,
            Err(error) => {
                self.discover.record_failure(&error.describe());
                cx.notify();
                return;
            }
        };
        self.discover.query = query;
        let stored = {
            let mut store = lock_catalog(&catalog);
            store.store_shadows(yagna_store::DISCOVER_LOGIN, &snapshots, Utc::now())
        };
        match stored {
            Ok(_) => {
                self.discover.results = snapshots;
                self.discover.record_success();
            }
            Err(error) => self.discover.record_failure(&error.to_string()),
        }
        self.refresh_search_budget();
        cx.notify();
    }

    /// Refreshes the search budget beside the DISCOVER controls from
    /// the free rate-limit probe.
    fn refresh_search_budget(&mut self) {
        let Some(host) = self.services.host.as_ref() else {
            return;
        };
        let token = self.services.token().map(|token| token.to_string());
        if let Ok(report) = host.rate_limit(token.as_deref()) {
            self.discover.remaining = Some(report.search_remaining);
        }
    }

    /// Opens a discovered or shadow repository's tab: the detail is
    /// built from the snapshot, and the README is enriched on demand
    /// through the forge — the endpoints are repo-addressed, not
    /// viewer-addressed, so any repository opens.
    fn open_shadow(&mut self, snapshot: RepoSnapshot, cx: &mut Context<Self>) {
        let repo = snapshot.repo_ref();
        let rowid = self.shadow_next;
        self.shadow_next -= 1;
        let index = self.tabs.open(repo.clone(), rowid);
        let health = yagna_core::health::health(&snapshot, self.now);
        let detail = yagna_core::model::RepoDetail {
            repo_rowid: rowid,
            repo_ref: repo.clone(),
            description: snapshot.description.clone(),
            stars: snapshot.stars,
            languages: snapshot.languages.clone(),
            license: snapshot.license.clone(),
            topics: snapshot.topics.clone(),
            default_branch: snapshot.default_branch.clone(),
            created_at: snapshot.created_at,
            last_commit_at: snapshot.last_commit_at,
            starred_at: None,
            first_commit_date: Some(snapshot.created_at),
            first_commit_source: Some(FIRST_COMMIT_SOURCE_DEFAULT.to_string()),
            commit_count: 0,
            latest_release: None,
            is_fork: snapshot.is_fork,
            is_archived: snapshot.is_archived,
            health,
            fit: None,
        };
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.data = Some(TabData {
                detail: Some(detail),
                readme: None,
                readme_note: None,
                tree: None,
                tree_nodes: Vec::new(),
                license: None,
                note: None,
                digest: None,
                assets: HashMap::new(),
                blocks: Vec::new(),
                tags: Vec::new(),
            });
            tab.images_started = true;
            tab.first_commit_asked = true;
            // The shadow's README is fetched live when the tab opens;
            // the store's on-demand path is the catalog's, not the
            // shadow's.
            tab.readme_asked = true;
            tab.license_asked = true;
        }
        cx.notify();
        let base = snapshot
            .default_branch
            .clone()
            .map(|branch| yagna_core::markdown::RawBase {
                owner: snapshot.owner.clone(),
                repo: snapshot.name.clone(),
                branch,
            });
        self.fetch_shadow_readme(rowid, repo, base, cx);
    }

    /// Fetches a shadow tab's README live: enrichment on demand for
    /// any repository, starred or not.
    fn fetch_shadow_readme(
        &mut self,
        rowid: i64,
        repo: yagna_core::model::RepoRef,
        base: Option<yagna_core::markdown::RawBase>,
        cx: &mut Context<Self>,
    ) {
        let services = Arc::clone(&self.services);
        let task = cx.background_executor().spawn(async move {
            let (api, host) = match (services.api.as_ref(), services.host.as_ref()) {
                (Some(api), Some(host)) => (api, host),
                _ => return Err("no token — set GITHUB_TOKEN to read".to_string()),
            };
            host.readme(api, &repo)
                .map_err(|error| error.describe())
                .map(|text| (base, text))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_shadow_readme(rowid, result, cx));
        })
        .detach();
    }

    /// Lands a shadow tab's README: rewritten against the repository's
    /// own raw base, parsed once, rendered after.
    fn apply_shadow_readme(
        &mut self,
        rowid: i64,
        result: Result<(Option<yagna_core::markdown::RawBase>, String), String>,
        cx: &mut Context<Self>,
    ) {
        let Ok((base, text)) = result else {
            return;
        };
        let Some(index) = self.tabs.index_of(rowid) else {
            return;
        };
        let fallback = yagna_core::markdown::RawBase {
            owner: String::new(),
            repo: String::new(),
            branch: "HEAD".to_string(),
        };
        let base = base.as_ref().unwrap_or(&fallback);
        let (rewritten, _) = yagna_core::markdown::rewrite_assets(&text, base);
        let empty: HashMap<String, yagna_core::mdrender::Resolved> = HashMap::new();
        let resolver = readme::TabResolver::new(&empty);
        let blocks = yagna_core::mdrender::render(&rewritten, &resolver);
        if let Some(tab) = self.tabs.tab_mut(index)
            && let Some(data) = tab.data.as_mut()
        {
            data.readme = Some(yagna_store::ReadmeBody {
                rewritten,
                truncated: false,
            });
            data.blocks = blocks;
        }
        cx.notify();
    }

    // --- The PEOPLE tab: the followed users' read-only shadows.

    /// Reads every followed login's stored shadows into the tab, and
    /// asks the forge who the reader could follow.
    fn load_people_cache(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let logins = self.settings.followed_users.clone();
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let mut cached: Vec<(String, Vec<ShadowRepo>, Option<String>)> = Vec::new();
            for login in &logins {
                let shadows = store.shadows_for(login).unwrap_or_default();
                let stamp = store
                    .followed_stamp(login)
                    .ok()
                    .flatten()
                    .map(|instant| instant.to_rfc3339());
                cached.push((login.clone(), shadows, stamp));
            }
            cached
        });
        cx.spawn(async move |this, cx| {
            let cached = fetch.await;
            let _ = this.update(cx, |pane, cx| {
                for (login, shadows, stamp) in cached {
                    pane.people.shadows.insert(login.clone(), shadows);
                    if let Some(stamp) = stamp {
                        pane.people.stamps.insert(login, stamp);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        self.load_suggestions(cx);
    }

    /// Fetches the logins the token's own user follows: the PEOPLE
    /// tab's suggestions, less the ones already followed.
    fn load_suggestions(&mut self, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let fetch = cx.background_executor().spawn(async move {
            let (api, host) = match (services.api.as_ref(), services.host.as_ref()) {
                (Some(api), Some(host)) => (api, host),
                _ => {
                    return Err("no token or runtime — cannot ask who to follow".to_string());
                }
            };
            host.following(api).map_err(|error| error.describe())
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.land_suggestions(result, cx));
        })
        .detach();
    }

    /// Lands the suggestions: the logins the reader could follow, or
    /// the reason there are none.
    fn land_suggestions(&mut self, result: Result<Vec<String>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(logins) => {
                let followed = self.settings.followed_users.clone();
                self.people.apply_suggestions(&logins, &followed);
            }
            Err(problem) => self.people.record_suggestion_failure(&problem),
        }
        cx.notify();
    }

    /// Follows one login: the settings list gains it, the store mirrors
    /// it, and its stars refresh on demand.
    fn follow_login(&mut self, cx: &mut Context<Self>) {
        let login = self.follow_field.value().trim().to_string();
        self.follow_field.clear();
        if login.is_empty() {
            return;
        }
        self.follow(login, cx);
    }

    /// Follows the named login — the field's way and the suggestion's
    /// way land in the same place.
    fn follow(&mut self, login: String, cx: &mut Context<Self>) {
        if !yagna_core::model::is_followable_login(&login) {
            self.people.note = Some(format!("cannot follow {login:?}: not a GitHub username"));
            cx.notify();
            return;
        }
        if !self
            .settings
            .followed_users
            .iter()
            .any(|held| held == &login)
        {
            self.settings.followed_users.push(login.clone());
            self.settings
                .followed_users
                .sort_by_key(|held| held.to_lowercase());
            self.save_settings(cx);
        }
        // The login is followed now, so it is not a suggestion any more.
        self.people.remove_suggestion(&login);
        let catalog = Arc::clone(&self.catalog);
        let stored_login = login.clone();
        let written = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.follow_user(&stored_login)
        });
        cx.spawn(async move |this, cx| {
            let _ = written.await;
            let _ = this.update(cx, |pane, cx| {
                pane.load_people_cache(cx);
                pane.refresh_person(&login, cx);
            });
        })
        .detach();
    }

    /// Unfollows one login: the settings list and the stored shadows go
    /// together.
    fn unfollow_login(&mut self, login: &str, cx: &mut Context<Self>) {
        self.settings.followed_users.retain(|held| held != login);
        self.save_settings(cx);
        self.people.drop_login(login);
        let gone = login.to_string();
        let catalog = Arc::clone(&self.catalog);
        let written = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.unfollow_user(&gone)
        });
        cx.spawn(async move |this, cx| {
            let _ = written.await;
            let _ = this.update(cx, |_pane, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    /// Refreshes one login's stars: their public list, stored as
    /// shadows with a fetched-at stamp. No auto-refresh — one user
    /// action, one fetch.
    fn refresh_person(&mut self, login: &str, cx: &mut Context<Self>) {
        if self.people.refreshing.is_some() {
            self.people.note = Some("one refresh at a time".to_string());
            cx.notify();
            return;
        }
        if self.services.api.is_none() {
            self.people.note = Some("no token — set GITHUB_TOKEN to refresh".to_string());
            cx.notify();
            return;
        }
        let services = Arc::clone(&self.services);
        let login = login.to_string();
        self.people.refreshing = Some(login.clone());
        self.people.note = None;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let (api, host) = match (services.api.as_ref(), services.host.as_ref()) {
                (Some(api), Some(host)) => (api, host),
                _ => {
                    return Err(yagna_forge::ForgeError::Parse(
                        "no token or runtime — cannot refresh".to_string(),
                    ));
                }
            };
            host.user_starred(api, &login)
                .map(|snapshots| (login, snapshots))
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_person(result, catalog, cx));
        })
        .detach();
    }

    /// Lands one login's refresh: stored as shadows, or the last fetch
    /// standing with a loud line when the network failed.
    fn apply_person(
        &mut self,
        result: Result<(String, Vec<RepoSnapshot>), yagna_forge::ForgeError>,
        catalog: Arc<Mutex<Store>>,
        cx: &mut Context<Self>,
    ) {
        let (login, snapshots) = match result {
            Ok(landed) => landed,
            Err(error) => {
                self.people.refreshing = None;
                self.people.note = Some(format!("offline — showing the last fetch ({error})"));
                cx.notify();
                return;
            }
        };
        let stored = {
            let mut store = lock_catalog(&catalog);
            store.store_shadows(&login, &snapshots, Utc::now())
        };
        self.people.refreshing = None;
        match stored {
            Ok(_) => {
                let shadows = snapshots
                    .into_iter()
                    .map(|snapshot| ShadowRepo {
                        login: login.clone(),
                        snapshot,
                        fetched_at: Utc::now(),
                    })
                    .collect::<Vec<_>>();
                self.people.apply_refresh(&login, shadows);
                self.people.note = None;
            }
            Err(error) => self.people.note = Some(error.to_string()),
        }
        cx.notify();
    }

    // --- The settings surface.

    /// Opens or closes the settings modal. Opening primes the two
    /// numeric fields with the values they commit back.
    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            let megabytes = self.settings.cache_cap_bytes / (1024 * 1024);
            self.cache_field.set(&megabytes.to_string());
            self.page_field.set(&self.settings.page_size.to_string());
            let folder = self.settings.local_folder.clone().unwrap_or_default();
            self.local_folder_field.set(&folder);
        }
        cx.notify();
    }

    /// One settings control committed: the change applies live, the
    /// file is written, and a write that fails says so.
    fn apply_settings_action(&mut self, action: SettingsAction, cx: &mut Context<Self>) {
        match action {
            SettingsAction::PickTheme(name) => self.apply_theme(&name, cx),
            SettingsAction::CycleSortDefault => {
                self.settings.sort_default = next_sort_key(self.settings.sort_default);
                self.set_sort(self.settings.sort_default, cx);
                self.save_settings(cx);
            }
            SettingsAction::CycleImagePolicy => {
                self.settings.image_policy = self.settings.image_policy.next();
                self.save_settings(cx);
            }
            SettingsAction::ToggleAutoUpdate => {
                self.settings.auto_update_on_launch = !self.settings.auto_update_on_launch;
                self.save_settings(cx);
            }
            SettingsAction::ToggleExactFirstCommit => {
                self.settings.exact_first_commit = !self.settings.exact_first_commit;
                self.save_settings(cx);
            }
            SettingsAction::TogglePersistExpansion => {
                self.settings.persist_expansion = !self.settings.persist_expansion;
                if self.settings.persist_expansion {
                    self.persist_expansion(cx);
                }
                self.save_settings(cx);
            }
        }
        cx.notify();
    }

    /// Applies a theme by name: the embedded default, overlaid by the
    /// user's override when there is one. A broken theme falls back and
    /// says so; the picker keeps the name that failed.
    fn apply_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        let palette = match theme::load_active(name, self.data_dir.as_deref()) {
            Ok(palette) => palette,
            Err(problem) => {
                self.notice = Some(problem);
                cx.notify();
                return;
            }
        };
        if let Err(problem) = theme::apply(&palette) {
            self.notice = Some(problem);
            cx.notify();
            return;
        }
        self.settings.theme = name.to_string();
        self.save_settings(cx);
    }

    /// Commits the two numeric fields: the page size and the cache cap.
    /// An empty or unparseable field keeps the value it had.
    fn commit_numeric_settings(&mut self, cx: &mut Context<Self>) {
        let page_typed = self.page_field.value().trim().to_string();
        let cache_typed = self.cache_field.value().trim().to_string();
        self.page_field.clear();
        self.cache_field.clear();
        if let Ok(page_size) = page_typed.parse::<u32>()
            && page_size > 0
        {
            self.settings.page_size = page_size;
        }
        if let Ok(megabytes) = cache_typed.parse::<u64>() {
            self.settings.cache_cap_bytes = megabytes * 1024 * 1024;
        }
        self.save_settings(cx);
        self.prune_cache(cx);
        self.page = 1;
        self.request_page(cx);
    }

    /// Commits the local folder: the setting is written, and the
    /// folder under it is scanned — the reader named a folder, and the
    /// repositories in it are what they asked for.
    fn commit_local_folder(&mut self, cx: &mut Context<Self>) {
        let typed = self.local_folder_field.value().trim().to_string();
        self.local_folder_field.clear();
        self.settings.local_folder = (!typed.is_empty()).then_some(typed);
        self.save_settings(cx);
        self.scan_local(cx);
    }

    /// Prunes the asset cache to the settings' cap, off the UI thread:
    /// the oldest-fetched images go first, the status line says what
    /// went when something did. The assets the open views are
    /// rendering — the tabs' images and the expanded rows' previews —
    /// are pinned and never evicted under an open view.
    fn prune_cache(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let cache_dir = self.cache_dir.clone();
        let cap = self.settings.cache_cap_bytes;
        let pinned = self.pinned_assets();
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.prune_cache(&cache_dir, cap, &pinned)
        });
        cx.spawn(async move |this, cx| {
            let report = work.await;
            let _ = this.update(cx, |pane, cx| pane.apply_prune(report, cx));
        })
        .detach();
    }

    /// The asset ids the open views render: every open tab's README
    /// assets plus the expanded rows' preview images. Plain strings —
    /// the one UI-shaped thing the store is ever told.
    fn pinned_assets(&self) -> Vec<String> {
        let mut pinned: Vec<String> = self
            .tabs
            .tabs()
            .iter()
            .filter_map(|tab| tab.data.as_ref())
            .flat_map(|data| data.assets.keys().cloned())
            .collect();
        pinned.extend(self.expanded.pinned_asset_ids());
        pinned
    }

    /// Lands the prune report: quiet when nothing was evicted, a status
    /// line when the cap bit.
    fn apply_prune(&mut self, report: Result<PruneReport, StoreError>, cx: &mut Context<Self>) {
        let Ok(report) = report else {
            self.notice = Some("the cache could not be pruned".to_string());
            cx.notify();
            return;
        };
        if report.evicted > 0 {
            self.notice = Some(format!(
                "the cache was pruned: {} image(s) ({}) evicted — {} file(s) ({}) remain",
                report.evicted,
                crate::paths::human_bytes(report.evicted_bytes),
                report.files - report.evicted,
                crate::paths::human_bytes(report.bytes - report.evicted_bytes),
            ));
            cx.notify();
        }
    }

    /// Writes the settings file; a failure is a status line, the
    /// session keeps running on what it has.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(problem) = crate::settings::save(&self.settings) {
            self.notice = Some(problem);
        }
        cx.notify();
    }

    /// The closures the settings modal's controls call.
    fn settings_actions(&self, cx: &mut Context<Self>) -> SettingsActions {
        let close = cx.listener(|pane: &mut Self, _: &ClickEvent, _, cx| pane.toggle_settings(cx));
        let action = cx.listener(|pane: &mut Self, action: &SettingsAction, _, cx| {
            pane.apply_settings_action(action.clone(), cx)
        });
        let cache_key = cx.listener(|pane: &mut Self, event: &KeyDownEvent, _, cx| {
            match pane.cache_field.on_key(event) {
                FieldAction::Submitted(_) => pane.commit_numeric_settings(cx),
                FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
                FieldAction::None => {}
            }
        });
        let page_key = cx.listener(|pane: &mut Self, event: &KeyDownEvent, _, cx| {
            match pane.page_field.on_key(event) {
                FieldAction::Submitted(_) => pane.commit_numeric_settings(cx),
                FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
                FieldAction::None => {}
            }
        });
        let local_key = cx.listener(|pane: &mut Self, event: &KeyDownEvent, _, cx| {
            match pane.local_folder_field.on_key(event) {
                FieldAction::Submitted(_) => pane.commit_local_folder(cx),
                FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
                FieldAction::None => {}
            }
        });
        let cache_focus = self.cache_field.focus().clone();
        let page_focus = self.page_field.focus().clone();
        let local_focus = self.local_folder_field.focus().clone();
        SettingsActions {
            on_close: Rc::new(move |window: &mut Window, cx: &mut App| {
                close(&ClickEvent::default(), window, cx)
            }),
            on_action: Rc::new(
                move |value: SettingsAction, window: &mut Window, cx: &mut App| {
                    action(&value, window, cx)
                },
            ),
            on_cache_key: Rc::new(move |event, window, cx| cache_key(event, window, cx)),
            on_cache_click: Rc::new(move |window, _| window.focus(&cache_focus)),
            on_page_key: Rc::new(move |event, window, cx| page_key(event, window, cx)),
            on_page_click: Rc::new(move |window, _| window.focus(&page_focus)),
            on_local_key: Rc::new(move |event, window, cx| local_key(event, window, cx)),
            on_local_click: Rc::new(move |window, _| window.focus(&local_focus)),
        }
    }

    // --- The fetch seam.

    /// Requests the current page from the store: one page call plus one
    /// total, run on the background executor — the UI thread never
    /// touches the database. With a query typed, the page is the ranked
    /// matches instead of the catalog.
    fn request_page(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let index = Arc::clone(&self.index);
        let sort = self.sort;
        let offset = self.offset();
        let page_size = self.page_size();
        let filter = self.sidebar.filter();
        let query = self.search.query().to_string();
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            // The clock starts inside the task: the executor's queueing
            // is not the query's cost.
            let began = Instant::now();
            let result = fetch_page(&store, &index, sort, offset, page_size, &filter, &query);
            (result, began.elapsed())
        });
        cx.spawn(async move |this, cx| {
            let (result, elapsed) = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_page(result, elapsed, cx));
        })
        .detach();
    }

    /// Lands a finished fetch: the new page replaces the old, caches
    /// reset, and a page number that ran past the end clamps and
    /// re-fetches once.
    fn apply_page(
        &mut self,
        result: Result<(Vec<RepoRow>, u64), StoreError>,
        elapsed: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        let (rows, total) = match result {
            Ok(landed) => landed,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.startup
            .mark_duration(crate::startup::Slot::Page, elapsed);
        self.total = total;
        if total == 0 {
            self.page = 1;
            self.rows.clear();
        } else {
            let last = self.page_count();
            if self.page > last {
                self.page = last;
                self.request_page(cx);
                return;
            }
        }
        self.rows = rows;
        self.displays = std::vec![None; self.rows.len()];
        self.excerpts.clear();
        self.list_state.reset(self.rows.len());
        self.focused_row = self.focused_row.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    /// Builds the fuzzy index: one read of the catalog's search rows,
    /// then the haystacks and the index itself. The build is measured —
    /// the status line reports it.
    fn build_index(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let began = Instant::now();
            let rows = store.search_index_rows();
            (rows, began.elapsed())
        });
        cx.spawn(async move |this, cx| {
            let (rows, elapsed) = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_index(rows, elapsed, cx));
        })
        .detach();
    }

    /// Lands the built index.
    fn apply_index(
        &mut self,
        rows: Result<Vec<SearchRow>, StoreError>,
        read_elapsed: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let Ok(rows) = rows else {
            self.index_note = "the search index could not be built".to_string();
            cx.notify();
            return;
        };
        let started = Instant::now();
        let items: Vec<SearchItem> = rows
            .iter()
            .map(|row| {
                let text = haystack(
                    &row.owner,
                    &row.name,
                    row.description.as_deref(),
                    &row.topics,
                    &row.tags,
                );
                SearchItem::new(row.repo_rowid, &text)
            })
            .collect();
        let count = items.len();
        *lock_index(&self.index) = FuzzyIndex::build(items);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        self.index_note = format!("indexed {count} repositories in {elapsed:.0} ms");
        self.startup.mark_duration(
            crate::startup::Slot::Index,
            read_elapsed + started.elapsed(),
        );
        self.index_ready = true;
        cx.notify();
    }

    /// Fetches the tag editor's answer off the render path: the edited
    /// row's tags and the ones it could carry.
    fn refresh_editor_tags(&mut self, cx: &mut Context<Self>) {
        let Some(rowid) = self.sidebar.edited() else {
            return;
        };
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let carried: Vec<(i64, String)> = store
                .repo_tags(rowid as u64)?
                .into_iter()
                .map(|tag| (tag.id, tag.name))
                .collect();
            let held: Vec<i64> = carried.iter().map(|(id, _)| *id).collect();
            let available: Vec<(i64, String)> = store
                .tags_with_counts(None)?
                .into_iter()
                .filter(|tag| !held.contains(&tag.id))
                .map(|tag| (tag.id, tag.name))
                .collect();
            Ok::<_, StoreError>((carried, available))
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_editor_tags(result, cx));
        })
        .detach();
    }

    /// Lands the editor's answer.
    fn apply_editor_tags(
        &mut self,
        result: Result<sidebar::EditorAnswer, StoreError>,
        cx: &mut Context<Self>,
    ) {
        let Ok((carried, available)) = result else {
            return;
        };
        self.sidebar
            .set_editor_tags(EditorTags { carried, available });
        cx.notify();
    }

    /// Refreshes the sidebar's lists and their counts: the tags, the
    /// catalog's aggregated topics, and the noted repositories.
    fn refresh_sidebar(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let filter = self.sidebar.filter();
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let tags = store.tags_with_counts(Some(TagKind::Tag))?;
            let topics = topic_counts(&store, &filter)?;
            let notes = store.notes_with_names()?;
            Ok::<SidebarLists, _>((tags, topics, notes))
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_sidebar_lists(result, cx));
        })
        .detach();
    }

    /// Lands the refreshed sidebar lists.
    fn apply_sidebar_lists(
        &mut self,
        result: Result<SidebarLists, StoreError>,
        cx: &mut Context<Self>,
    ) {
        if let Ok((tags, topics, notes)) = result {
            self.sidebar.set_tags(tags);
            self.sidebar.set_topics(topics);
            self.sidebar.set_notes(notes);
        }
        cx.notify();
    }

    // --- The row view models.

    /// The view model of one row, built on first paint and cached: the
    /// static strings come from the page landing; the excerpt is shaped
    /// against the current content width once per row per width. The
    /// scroll path reuses these, never re-shapes per frame.
    fn display_for(&mut self, index: usize, window: &Window) -> RowDisplay {
        if self.displays.len() != self.rows.len() {
            self.displays = std::vec![None; self.rows.len()];
        }
        if let Some(display) = &self.displays[index] {
            return display.clone();
        }
        let excerpt = self.excerpt_for(index, window);
        let display = match self.rows.get(index) {
            Some(row) => {
                let mut display = build_display(row, self.now);
                display.excerpt = excerpt;
                display
            }
            None => build_display(&stub_row(), self.now),
        };
        self.displays[index] = Some(display.clone());
        display
    }

    /// The shaped excerpt of one row, from the per-width cache.
    fn excerpt_for(&mut self, index: usize, window: &Window) -> rows::ExcerptLines {
        let width = RowLayout::content_width(window.viewport_size().width);
        let key = (index, f32::from(width).max(0.0) as u32);
        if let Some(cached) = self.excerpts.get(&key) {
            return cached.clone();
        }
        let text = self
            .rows
            .get(index)
            .map(|row| row.excerpt.as_str())
            .unwrap_or_default();
        let shaped = shape_excerpt(text, width, window);
        self.excerpts.insert(key, shaped.clone());
        shaped
    }

    /// The position of a repository in the current page.
    fn index_of(&self, repo_rowid: i64) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.repo_rowid == repo_rowid)
    }

    // --- The expansion.

    /// Opens or closes a row: the chevron turns, the row grows, and the
    /// preview is fetched once.
    fn toggle_expanded(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let repo_rowid = row.repo_rowid;
        let open = self.expanded.toggle(repo_rowid);
        if open {
            self.request_preview(repo_rowid, cx);
        }
        self.persist_expansion(cx);
        self.list_state.splice(index..index + 1, 1);
        cx.notify();
    }

    /// Fetches a row's preview: the README's head and the image. The
    /// image policy decides whether the row fetches at all.
    fn request_preview(&mut self, repo_rowid: i64, cx: &mut Context<Self>) {
        if !self.settings.image_policy.row_may_fetch() {
            return;
        }
        if !self.expanded.begin(repo_rowid) {
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let cache_dir = self.cache_dir.clone();
        let fetch = cx
            .background_executor()
            .spawn(async move { read_preview(&catalog, &services, &cache_dir, repo_rowid) });
        cx.spawn(async move |this, cx| {
            let preview = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_preview(repo_rowid, preview, cx));
        })
        .detach();
    }

    /// Lands a fetched preview and re-measures the row it belongs to.
    /// A preview with no README head asks the forge for that one
    /// repository's README — the row re-reads itself when it lands.
    fn apply_preview(
        &mut self,
        repo_rowid: i64,
        preview: Result<RowPreview, String>,
        cx: &mut Context<Self>,
    ) {
        self.expanded.finish(repo_rowid);
        if let Ok(preview) = preview {
            self.expanded.set_preview(repo_rowid, preview);
            if let Some(index) = self.index_of(repo_rowid) {
                self.list_state.splice(index..index + 1, 1);
            }
        }
        self.ask_row_readme(repo_rowid, cx);
        cx.notify();
    }

    /// Asks the forge for one row's missing README, on demand: one
    /// request per row, debounced by the queue's own enqueue — a
    /// second ask while one is in flight fetches nothing. The store's
    /// write is the enrichment pipeline's own, so the row, the tab,
    /// the FTS index and the digests all see it.
    fn ask_row_readme(&mut self, repo_rowid: i64, cx: &mut Context<Self>) {
        if !self.expanded.is_open(repo_rowid) {
            return;
        }
        if self
            .expanded
            .preview(repo_rowid)
            .is_some_and(|preview| preview.total_lines > 0)
        {
            return;
        }
        if !self.readme_asked_rows.insert(repo_rowid) {
            return;
        }
        let Some(row) = self.rows.iter().find(|row| row.repo_rowid == repo_rowid) else {
            return;
        };
        let repo = row.repo_ref.clone();
        self.readme_fetching_rows.insert(repo_rowid);
        self.fetch_row_readme(repo_rowid, repo, cx);
    }

    /// The row's README fetch, off the UI thread.
    fn fetch_row_readme(
        &mut self,
        repo_rowid: i64,
        repo: yagna_core::model::RepoRef,
        cx: &mut Context<Self>,
    ) {
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let task = cx.background_executor().spawn(async move {
            workers::fetch_readme(&catalog, &services, repo_rowid as u64, &repo).await
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| {
                pane.apply_row_readme(repo_rowid, outcome, cx)
            });
        })
        .detach();
    }

    /// Lands a row's on-demand README: the row re-reads itself with the
    /// body now stored, or says why in one line and leaves the row
    /// standing.
    fn apply_row_readme(
        &mut self,
        repo_rowid: i64,
        outcome: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        self.readme_fetching_rows.remove(&repo_rowid);
        match outcome {
            Ok(()) => {
                self.expanded.forget(repo_rowid);
                self.request_preview(repo_rowid, cx);
            }
            Err(why) => {
                self.readme_failed_rows.insert(repo_rowid, why);
                if let Some(index) = self.index_of(repo_rowid) {
                    self.list_state.splice(index..index + 1, 1);
                }
            }
        }
        cx.notify();
    }

    // --- The search.

    /// The search field's key handling: enter (or the search button)
    /// ranks the catalog — the ranking is deliberate, not per
    /// keystroke.
    fn on_search_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.search_field.on_key(event) {
            FieldAction::Changed | FieldAction::Moved => cx.notify(),
            FieldAction::Escaped => self.clear_query(cx),
            FieldAction::Submitted(_) => self.run_query(cx),
            FieldAction::None => {}
        }
    }

    /// Runs the search with what the field holds. The search is not run
    /// on every letter: the reader asks for it, and the results arrive
    /// when they arrive.
    fn run_query(&mut self, cx: &mut Context<Self>) {
        self.search.set(self.search_field.value());
        self.page = 1;
        self.focused_row = 0;
        self.request_page(cx);
    }

    /// Empties the search and shows the whole catalog again.
    fn clear_query(&mut self, cx: &mut Context<Self>) {
        self.search.set("");
        self.page = 1;
        self.focused_row = 0;
        self.request_page(cx);
    }

    // --- The deep search.

    /// Opens or closes the deep-search panel. Closing leaves the
    /// catalog as it is — the panel is a search over text, not a view
    /// mode.
    fn toggle_deep(&mut self, cx: &mut Context<Self>) {
        self.deep_open = !self.deep_open;
        self.deep_note = None;
        cx.notify();
    }

    /// The deep field's key handling: the field's own meaning first,
    /// then the panel's — enter runs (or opens the selection), the
    /// arrows walk it, escape closes.
    fn on_deep_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.deep_open = false;
            self.deep_note = None;
            cx.notify();
            return;
        }
        if key == "down" || key == "up" {
            self.walk_deep_selection(if key == "down" { 1 } else { -1 }, cx);
            return;
        }
        match self.deep_field.on_key(event) {
            FieldAction::Submitted(_) => {
                // Enter re-runs a revised query: the hits on screen
                // belong to the query that made them, and a new one
                // must not open the old selection.
                let revised = self.deep_field.value().trim().to_string();
                if self.deep_hits.is_empty() || revised != self.deep_query {
                    self.run_deep_query(cx);
                } else {
                    self.open_deep_selection(cx);
                }
            }
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => {
                self.builder.sync_from_text(self.deep_field.value());
                cx.notify();
            }
            FieldAction::None => {}
        }
    }

    /// The builder's controls report: what each one did.
    fn builder_actions(&self, cx: &mut Context<Self>) -> builder::BuilderActions {
        let on_add = cx.listener(|pane: &mut Self, _, _, cx| pane.add_builder_clause(cx));
        let on_remove = cx.listener(|pane: &mut Self, index: &usize, _, cx| {
            pane.remove_builder_clause(*index, cx)
        });
        let on_value_key =
            cx.listener(|pane: &mut Self, event, _, cx| pane.on_builder_key(event, cx));
        let on_value_click = cx.listener(|pane: &mut Self, _, window, _| {
            let focus = pane.builder_field.focus().clone();
            window.focus(&focus);
        });
        builder::BuilderActions {
            on_add: Rc::new(move |window: &mut Window, cx: &mut App| {
                on_add(&ClickEvent::default(), window, cx)
            }),
            on_remove: Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                on_remove(&index, window, cx)
            }),
            on_value_key: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    on_value_key(event, window, cx)
                },
            ),
            on_value_click: Rc::new(move |window: &mut Window, cx: &mut App| {
                on_value_click(&ClickEvent::default(), window, cx)
            }),
        }
    }

    /// A keystroke in the builder's value field: the text goes into
    /// the draft, and enter adds the clause.
    fn on_builder_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match self.builder_field.on_key(event) {
            field::FieldAction::Submitted(_) => self.add_builder_clause(cx),
            field::FieldAction::Changed | field::FieldAction::Moved => {
                let value = self.builder_field.value().to_string();
                self.builder.set_draft_value(&value);
                cx.notify();
            }
            field::FieldAction::Escaped => {
                self.builder_field.clear();
                self.builder.set_draft_value("");
                cx.notify();
            }
            field::FieldAction::None => {}
        }
    }

    /// Adds the builder's draft clause to the query. The text field
    /// stays the source of truth: the composed text goes back into it.
    fn add_builder_clause(&mut self, cx: &mut Context<Self>) {
        let value = self.builder_field.value().trim().to_string();
        self.builder.set_draft_value(&value);
        let text = self.deep_field.value().to_string();
        match self.builder.add_draft(&text) {
            Ok(composed) => {
                self.deep_field.set(&composed);
                self.builder.sync_from_text(&composed);
                self.builder_field.clear();
                self.builder.set_draft_value("");
                self.deep_note = None;
            }
            Err(refusal) => self.deep_note = Some(refusal.clone()),
        }
        cx.notify();
    }

    /// Removes the clause at the index from the query.
    fn remove_builder_clause(&mut self, index: usize, cx: &mut Context<Self>) {
        let text = self.deep_field.value().to_string();
        match self.builder.remove(&text, index) {
            Ok(composed) => {
                self.deep_field.set(&composed);
                self.builder.sync_from_text(&composed);
                self.deep_note = None;
            }
            Err(refusal) => self.deep_note = Some(refusal.clone()),
        }
        cx.notify();
    }

    /// Moves the deep selection up or down.
    fn walk_deep_selection(&mut self, step: i64, cx: &mut Context<Self>) {
        if self.deep_hits.is_empty() {
            return;
        }
        let last = self.deep_hits.len() as i64 - 1;
        let next = (self.deep_selected as i64 + step).clamp(0, last);
        self.deep_selected = next as usize;
        cx.notify();
    }

    /// Runs the deep search with what the field holds: the core parses
    /// first — a bad query is an answer on the spot, never a store
    /// error.
    fn run_deep_query(&mut self, cx: &mut Context<Self>) {
        let text = self.deep_field.value().trim().to_string();
        self.deep_query = text.clone();
        let Ok(parsed) = query::parse(&text) else {
            self.deep_note = Some("the query does not parse — try reading lang:rust".to_string());
            self.deep_hits.clear();
            self.deep_selected = 0;
            cx.notify();
            return;
        };
        self.deep_running = true;
        self.deep_note = None;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.search(&parsed, DEEP_LIMIT)
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_deep_hits(result, cx));
        })
        .detach();
    }

    /// Lands the deep search's hits.
    fn apply_deep_hits(
        &mut self,
        result: Result<Vec<SearchHit>, StoreError>,
        cx: &mut Context<Self>,
    ) {
        self.deep_running = false;
        match result {
            Ok(hits) => {
                self.deep_note = if hits.is_empty() {
                    Some("nothing in the READMEs or the digests matches".to_string())
                } else {
                    None
                };
                self.deep_hits = hits;
            }
            Err(error) => self.deep_note = Some(error.to_string()),
        }
        self.deep_selected = 0;
        cx.notify();
    }

    /// Opens the selected hit: README hits open the tab at its README,
    /// digest hits with the digest section showing.
    fn open_deep_selection(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.deep_hits.get(self.deep_selected) else {
            return;
        };
        let rowid = hit.row.repo_rowid;
        let at_digest = hit.source == SearchSource::Digest;
        let repo = hit.row.repo_ref.clone();
        self.open_tab_at(repo, rowid, at_digest, cx);
    }

    /// The list's key map: the slash focuses the search, the arrows
    /// walk the rows, and enter opens the focused row's tab.
    fn on_list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            // Escape is the modal's way out; the other keys belong to
            // its fields.
            if event.keystroke.key == "escape" {
                self.settings_open = false;
                cx.notify();
            }
            return;
        }
        if self.dialog.is_some() {
            // Escape closes the shell's card; the other keys belong to
            // the window behind it, not to the card.
            if event.keystroke.key == "escape" {
                self.dialog = None;
                cx.notify();
            }
            return;
        }
        if self.search_field.focus().is_focused(window)
            || self.jump_field.focus().is_focused(window)
            || self.tag_field.focus().is_focused(window)
            || self.rename_field.focus().is_focused(window)
            || self.sidebar_filter_field.focus().is_focused(window)
            || self.note_field.focus().is_focused(window)
            || self.deep_field.focus().is_focused(window)
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        let control = event.keystroke.modifiers.control;
        if control && key == "w" {
            self.close_tab(cx);
            return;
        }
        if control && key == "r" {
            self.focus_surface(tabs::Surface::Library, cx);
            return;
        }
        if control && key == "tab" {
            self.tabs.cycle(true);
            cx.notify();
            return;
        }
        if control && key == "shift-tab" {
            self.tabs.cycle(false);
            cx.notify();
            return;
        }
        if control && key == "," {
            self.toggle_settings(cx);
            return;
        }
        match key {
            "/" => window.focus(self.search_field.focus()),
            "up" => self.walk_focus(-1, cx),
            "down" => self.walk_focus(1, cx),
            "enter" => self.open_focused_tab(cx),
            _ => {}
        }
    }

    /// Moves the focused row up or down.
    fn walk_focus(&mut self, step: i64, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as i64 - 1;
        let next = (self.focused_row as i64 + step).clamp(0, last);
        self.focused_row = next as usize;
        cx.notify();
    }

    // --- The tags.

    /// Opens the sidebar's editor for one row — the add-tag chip.
    fn edit_tags(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.sidebar.edit(row.repo_rowid);
        self.refresh_editor_tags(cx);
        self.tag_field.clear();
        cx.notify();
    }

    /// Assigns a tag to the edited row and patches the fuzzy index. The
    /// kind is the editor's: a plain tag, or a category.
    fn assign_tag(
        &mut self,
        repo_rowid: i64,
        name: &str,
        kind: Option<TagKind>,
        cx: &mut Context<Self>,
    ) {
        let catalog = Arc::clone(&self.catalog);
        let name = name.to_string();
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.assign_tag(repo_rowid as u64, &name, kind)?;
            let row = store.search_index_row(repo_rowid)?;
            Ok::<_, StoreError>((repo_rowid, row))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |pane, cx| pane.apply_tag(result, cx));
        })
        .detach();
    }

    /// Removes a tag from a row, by name — the chip's click.
    fn remove_tag_by_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let name = name.to_string();
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            let Some(tag_id) = store.tag_id_by_name(&name)? else {
                return Ok((0i64, None));
            };
            let repo_rowid = store.repo_rowid_of_tag(tag_id)?;
            store.unassign_tag(repo_rowid as u64, tag_id)?;
            let row = store.search_index_row(repo_rowid)?;
            Ok::<_, StoreError>((repo_rowid, row))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |pane, cx| pane.apply_tag(result, cx));
        })
        .detach();
    }

    /// Lands a tag change: the index is patched, the page and the tag
    /// lists are refreshed.
    fn apply_tag(
        &mut self,
        result: Result<(i64, Option<SearchRow>), StoreError>,
        cx: &mut Context<Self>,
    ) {
        if let Ok((_, Some(row))) = result {
            let text = haystack(
                &row.owner,
                &row.name,
                row.description.as_deref(),
                &row.topics,
                &row.tags,
            );
            lock_index(&self.index).patch(row.repo_rowid, &text);
        }
        self.refresh_sidebar(cx);
        self.refresh_editor_tags(cx);
        self.request_page(cx);
    }

    /// The sidebar's actions.
    fn apply_sidebar_action(&mut self, action: SidebarAction, cx: &mut Context<Self>) {
        match action {
            SidebarAction::ToggleTag(tag_id) => {
                self.sidebar.toggle(tag_id);
                self.page = 1;
                self.focused_row = 0;
                self.request_page(cx);
            }
            SidebarAction::ToggleTopic(topic) => {
                self.sidebar.toggle_topic(&topic);
                self.page = 1;
                self.focused_row = 0;
                self.request_page(cx);
                self.refresh_sidebar(cx);
            }
            SidebarAction::ToggleMode => {
                self.sidebar.toggle_mode();
                self.page = 1;
                self.request_page(cx);
            }
            SidebarAction::ClearFilter => {
                let dropped = self.sidebar.clear_selection();
                self.sidebar
                    .clear_arms(&format!("cleared {dropped} filter(s)"));
                self.page = 1;
                self.focused_row = 0;
                self.request_page(cx);
                self.refresh_sidebar(cx);
            }
            SidebarAction::ToggleDrawer(drawer) => {
                self.sidebar.toggle_drawer(drawer);
                if drawer == DrawerKind::Activity {
                    self.load_activity(cx);
                }
                self.persist_drawer(cx);
                cx.notify();
            }
            SidebarAction::OpenNote(subject) => self.open_noted(subject, cx),
            SidebarAction::CloseEditor => {
                self.sidebar.close_editor();
                self.tag_field.clear();
                cx.notify();
            }
            SidebarAction::AddTag { name } => {
                let Some(repo_rowid) = self.sidebar.edited() else {
                    return;
                };
                self.tag_field.clear();
                self.assign_tag(repo_rowid, &name, Some(TagKind::Tag), cx);
            }
            SidebarAction::AttachTag(name) => {
                let Some(repo_rowid) = self.sidebar.edited() else {
                    return;
                };
                self.assign_tag(repo_rowid, &name, None, cx);
            }
            SidebarAction::RemoveTag(tag_id) => {
                let Some(repo_rowid) = self.sidebar.edited() else {
                    return;
                };
                self.unassign_tag(repo_rowid, tag_id, cx);
            }
            SidebarAction::ArmDelete(tag_id) => {
                let count = self.tag_count(tag_id);
                self.sidebar.arm_delete(tag_id, count);
                cx.notify();
            }
            SidebarAction::ConfirmDelete(tag_id) => {
                self.sidebar.clear_arms("the tag was deleted");
                self.delete_tag(tag_id, cx);
            }
            SidebarAction::CancelDelete => {
                self.sidebar.clear_arms("the delete was cancelled");
                cx.notify();
            }
            SidebarAction::ArmMerge(tag_id) => {
                let Some(name) = self.tag_name(tag_id) else {
                    return;
                };
                let count = self.tag_count(tag_id);
                self.sidebar.arm_merge(tag_id, &name, count);
                cx.notify();
            }
            SidebarAction::MergeInto { source, target } => {
                let count = self.tag_count(source);
                let name = self.tag_name(source).unwrap_or_default();
                self.sidebar.clear_arms("the tags were merged");
                self.merge_tags(source, target, count, &name, cx);
            }
            SidebarAction::ArmRename(tag_id) => {
                let Some(name) = self.tag_name(tag_id) else {
                    return;
                };
                self.sidebar.arm_rename(tag_id);
                self.rename_field.set(&name);
                cx.notify();
            }
            SidebarAction::Rename { tag_id, name } => {
                self.rename_field.clear();
                self.sidebar.clear_arms("the tag was renamed");
                self.rename_tag(tag_id, &name, cx);
            }
        }
    }

    /// The click-outside rule, one place: a click that reaches the root
    /// was outside every dropdown and every inline editor (each of
    /// those stops its own clicks), so the open dropdown closes, the
    /// armed rename/merge/delete is cancelled, and the sidebar's tag
    /// editor and the note editor let go. ESC inside a field still
    /// works — this is the addition, not the replacement.
    fn cancel_if_outside(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.dropdown.take().is_some();
        if self.sidebar.has_arms() {
            self.sidebar.clear_arms("the edit was cancelled");
            changed = true;
        }
        if self.sidebar.edited().is_some() {
            self.sidebar.close_editor();
            self.tag_field.clear();
            changed = true;
        }
        if self.note_editing.take().is_some() {
            self.note_field.clear();
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    // --- The note's editor: the popup over the page.

    /// Opens the focused tab's note for editing — the tab's own "edit"
    /// affordance. The editor is a popup: a paragraph, a save and a
    /// delete, and no length to it.
    fn edit_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(subject) = self.note_subject() else {
            return;
        };
        let existing = self.note_text(subject);
        self.note_editing = Some(subject);
        self.note_field.set(&existing);
        self.note_field.take_focus(window);
        cx.notify();
    }

    /// The subject the focused tab's note is about: a local entry for a
    /// local tab, the catalog's row for the rest.
    fn note_subject(&self) -> Option<NoteSubject> {
        let tab = self.tabs.focused()?;
        if let Some(local) = tab.local.as_ref() {
            return Some(NoteSubject::Local(local.id));
        }
        if tab.rowid < 0 {
            return None;
        }
        Some(NoteSubject::Repo(tab.rowid))
    }

    /// The note the subject carries, as the tab holds it.
    fn note_text(&self, subject: NoteSubject) -> String {
        let Some(tab) = self.tabs.focused() else {
            return String::new();
        };
        match subject {
            NoteSubject::Repo(_) => tab
                .data
                .as_ref()
                .and_then(|data| data.note.clone())
                .unwrap_or_default(),
            NoteSubject::Local(id) => tab
                .local
                .as_ref()
                .filter(|local| local.id == id)
                .and_then(|local| local.note.clone())
                .unwrap_or_default(),
        }
    }

    /// The note field's key handling: escape closes the popup, every
    /// other keystroke is the reader's own words — except ctrl+enter,
    /// which saves (the paragraph's own enter breaks the line). Ctrl-V
    /// and Ctrl-C reach the field's clipboard handling first.
    fn on_note_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if is_save_key(event) {
            self.save_note(cx);
            return;
        }
        match self.note_field.on_key_clipboard(event, cx) {
            FieldAction::Escaped => self.close_note(cx),
            FieldAction::Submitted(_)
            | FieldAction::Changed
            | FieldAction::Moved
            | FieldAction::None => cx.notify(),
        }
    }

    /// Saves the note: one write, the note belongs to its subject, and
    /// a re-sync never touches it. An empty note deletes it.
    fn save_note(&mut self, cx: &mut Context<Self>) {
        self.write_note(self.note_field.value().to_string(), cx);
    }

    /// Deletes the note: the subject keeps no note at all.
    fn delete_note(&mut self, cx: &mut Context<Self>) {
        self.write_note(String::new(), cx);
    }

    /// Writes one subject's note, off the UI thread, and closes the
    /// popup.
    fn write_note(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(subject) = self.note_editing.take() else {
            return;
        };
        self.note_field.clear();
        let catalog = Arc::clone(&self.catalog);
        let write = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.set_note(subject, &text, Utc::now())
        });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            let _ = this.update(cx, |pane, cx| {
                if let Err(problem) = written {
                    pane.notice = Some(problem.to_string());
                }
                pane.refresh_sidebar(cx);
                pane.reload_note(subject, cx);
            });
        })
        .detach();
    }

    /// Closes the note's popup without saving.
    fn close_note(&mut self, cx: &mut Context<Self>) {
        self.note_editing = None;
        self.note_field.clear();
        cx.notify();
    }

    /// Reads the note back after it was written: the tab that carries
    /// it shows the reader's words again.
    fn reload_note(&mut self, subject: NoteSubject, cx: &mut Context<Self>) {
        match subject {
            NoteSubject::Repo(rowid) => self.load_tab(rowid, cx),
            NoteSubject::Local(id) => {
                let Some(index) = self.tabs.index_of_local(id) else {
                    return;
                };
                self.load_local_tab(index, cx);
            }
        }
    }

    // --- The local repositories: the clones on this machine.

    /// Scans the folder the settings name: the worktrees under it are
    /// found, written to the store, and matched to the catalog's rows.
    /// The scan is a background task — the catalog's own queries are
    /// never blocked by it.
    fn scan_local(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.settings.local_folder.clone() else {
            self.notice = Some("no folder is set — name one in the settings".to_string());
            cx.notify();
            return;
        };
        self.local_scanning = true;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let path = PathBuf::from(&folder);
        let task = cx.background_executor().spawn(async move {
            let found = yagna_store::scan_folder(&path);
            let mut store = lock_catalog(&catalog);
            match found {
                Ok(scans) => {
                    store
                        .replace_local_scan(&folder, &scans, Utc::now())
                        .map_err(|error| error.to_string())?;
                    store.local_repos().map_err(|error| error.to_string())
                }
                Err(problem) => Err(problem.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_local_scan(result, cx));
        })
        .detach();
    }

    /// Lands the scan: the local list is the store's, and the status
    /// line says how many repositories were found.
    fn apply_local_scan(&mut self, result: Result<Vec<LocalRepo>, String>, cx: &mut Context<Self>) {
        self.local_scanning = false;
        match result {
            Ok(repos) => {
                self.notice = Some(format!(
                    "{} local {} found",
                    repos.len(),
                    if repos.len() == 1 {
                        "repository"
                    } else {
                        "repositories"
                    }
                ));
                self.local_repos = repos;
            }
            Err(problem) => {
                self.notice = Some(format!("the folder could not be scanned — {problem}"));
            }
        }
        cx.notify();
    }

    /// Reads the local repositories the last scan found — the surface's
    /// list, off the UI thread.
    fn load_local_list(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.local_repos().map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_local_list(result, cx));
        })
        .detach();
    }

    /// Lands the local list: the surface shows what the scan found.
    fn apply_local_list(&mut self, result: Result<Vec<LocalRepo>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(repos) => self.local_repos = repos,
            Err(problem) => self.notice = Some(problem),
        }
        cx.notify();
    }

    /// Opens a local repository's page: the tab reads the worktree.
    /// The entry comes from the surface's list when it is there; when
    /// it is not — the local surface was never opened — the store
    /// answers instead, so the notes drawer can open one too.
    fn open_local_repo(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(repo) = self.local_repos.iter().find(|repo| repo.id == id).cloned() {
            let index = self.tabs.open_local(repo);
            self.load_local_tab(index, cx);
            cx.notify();
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            store.local_repo(id).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_local_open(result, cx));
        })
        .detach();
    }

    /// Lands a local entry the store answered with, and opens it.
    fn apply_local_open(
        &mut self,
        result: Result<Option<LocalRepo>, String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(Some(repo)) = result else {
            return;
        };
        let index = self.tabs.open_local(repo);
        self.load_local_tab(index, cx);
        cx.notify();
    }

    /// Reads a local tab's content: the entry itself, the catalog's row
    /// when the origin matched one, and the reader's note.
    fn load_local_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        let Some(local) = tab.local.as_ref() else {
            return;
        };
        let id = local.id;
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let repo = store.local_repo(id)?;
            let linked = match repo.as_ref().and_then(|repo| repo.linked_rowid) {
                Some(rowid) => store.repo_detail(rowid as u64)?,
                None => None,
            };
            let note = store.note_of(NoteSubject::Local(id))?;
            // The root's own entries: the tree opens with them.
            let entries = match repo.as_ref() {
                Some(repo) => yagna_store::read_directory(Path::new(&repo.path), "").ok(),
                None => None,
            };
            Ok::<_, StoreError>(LocalTabContent {
                repo,
                linked,
                note,
                entries,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_local_tab(index, result, cx));
        })
        .detach();
    }

    /// Lands the local tab's content: the worktree's own facts, the
    /// catalog's row when there is one, and the note.
    fn apply_local_tab(
        &mut self,
        index: usize,
        result: Result<LocalTabContent, StoreError>,
        cx: &mut Context<Self>,
    ) {
        let Ok(content) = result else {
            self.notice = Some("the local repository could not be read".to_string());
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.tab_mut(index) else {
            return;
        };
        let Some(local) = tab.local.as_mut() else {
            return;
        };
        if let Some(repo) = content.repo {
            local.repo = repo;
        }
        local.linked = content.linked;
        local.note = content.note;
        if let Some(entries) = content.entries {
            local.entries.insert(String::new(), entries);
        }
        cx.notify();
    }

    /// Opens or closes a folder of the worktree: its entries are read
    /// from the disk, off the UI thread.
    fn toggle_local_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        let Some(local) = tab.local.as_ref() else {
            return;
        };
        if local.opened.contains(path) {
            if let Some(tab) = self.tabs.tab_mut(index)
                && let Some(local) = tab.local.as_mut()
            {
                local.opened.remove(path);
            }
            cx.notify();
            return;
        }
        let root = local.repo.path.clone();
        let relative = path.to_string();
        let asked = relative.clone();
        let task = cx.background_executor().spawn(async move {
            yagna_store::read_directory(Path::new(&root), &asked).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                pane.apply_local_folder(index, relative, result, cx)
            });
        })
        .detach();
    }

    /// Lands a folder's entries: the folder opens with them.
    fn apply_local_folder(
        &mut self,
        index: usize,
        path: String,
        result: Result<Vec<yagna_core::model::LocalEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(entries) = result else {
            self.notice = Some("that folder could not be read".to_string());
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.tab_mut(index) else {
            return;
        };
        if let Some(local) = tab.local.as_mut() {
            local.opened.insert(path.clone());
            local.entries.insert(path, entries);
        }
        cx.notify();
    }

    /// Opens a file of the worktree: its text is read, off the UI
    /// thread.
    fn open_local_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        let Some(local) = tab.local.as_ref() else {
            return;
        };
        let root = local.repo.path.clone();
        let relative = path.to_string();
        let asked = relative.clone();
        let task = cx.background_executor().spawn(async move {
            yagna_store::read_preview(Path::new(&root), &asked).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                pane.apply_local_file(index, relative, result, cx)
            });
        })
        .detach();
    }

    /// Lands a file's text: the page shows it.
    fn apply_local_file(
        &mut self,
        index: usize,
        path: String,
        result: Result<yagna_store::LocalPreview, String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(preview) = result else {
            self.notice = Some("that file could not be read".to_string());
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.tab_mut(index) else {
            return;
        };
        if let Some(local) = tab.local.as_mut() {
            local.preview = Some(preview);
            local.preview_path = Some(path);
        }
        cx.notify();
    }

    /// The closures the local tab's page calls.
    fn local_tab_actions(&self, cx: &mut Context<Self>) -> LocalTabActions {
        let folder =
            cx.listener(|pane: &mut Self, path: &str, _, cx| pane.toggle_local_folder(path, cx));
        let file = cx.listener(|pane: &mut Self, path: &str, _, cx| pane.open_local_file(path, cx));
        let note = cx.listener(|pane: &mut Self, _, window, cx| pane.edit_note(window, cx));
        LocalTabActions {
            on_toggle_folder: Rc::new(move |path, window, cx| folder(path, window, cx)),
            on_open_file: Rc::new(move |path, window, cx| file(path, window, cx)),
            on_edit_note: Rc::new(move |window, cx| note(&(), window, cx)),
        }
    }

    /// The closures the local surface calls.
    fn local_surface_actions(&self, cx: &mut Context<Self>) -> LocalSurfaceActions {
        let scan = cx.listener(|pane: &mut Self, _, _, cx| pane.scan_local(cx));
        let open = cx.listener(|pane: &mut Self, id: &i64, _, cx| pane.open_local_repo(*id, cx));
        LocalSurfaceActions {
            on_scan: Rc::new(move |window, cx| scan(&(), window, cx)),
            on_open: Rc::new(move |id, window, cx| open(&id, window, cx)),
        }
    }

    /// The local surface: the repositories that live on this machine.
    fn render_local_overlay(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let actions = self.local_surface_actions(cx);
        let content = localsurface::render(
            &self.local_repos,
            self.settings.local_folder.as_deref(),
            self.local_scanning,
            &actions,
        );
        div().size_full().bg(theme::background()).child(content)
    }

    /// The score surface: the fit spec — editable here — the
    /// catalogue, and the two score spreads.
    fn render_score_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let actions = self.score_surface_actions(cx);
        let content = match self.score_view.as_ref() {
            Some(stored) => {
                let mut view = stored.clone();
                view.busy = self.score_busy;
                if self.score_note.is_some() {
                    view.note = self.score_note.clone();
                }
                scoresurface::render(&view, &actions, &self.spec_field, window)
            }
            None => div()
                .id("score-surface-loading")
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child("reading the score facts…"),
        };
        div().size_full().bg(theme::background()).child(content)
    }

    fn score_surface_actions(&self, cx: &mut Context<Self>) -> scoresurface::ScoreActions {
        let rescore = cx.listener(|pane: &mut Self, _, _, cx| pane.start_rescore(cx));
        let open_folder = cx.listener(|pane: &mut Self, _, _, cx| pane.open_data_folder(cx));
        let apply = cx.listener(|pane: &mut Self, _, _, cx| pane.apply_spec_edit(cx));
        let revert = cx.listener(|pane: &mut Self, _, _, cx| pane.revert_spec_field(cx));
        let spec_key = cx.listener(|pane: &mut Self, event: &KeyDownEvent, window, cx| {
            pane.on_spec_field_key(event, window, cx)
        });
        let spec_seek = cx.listener(|pane: &mut Self, index: &usize, window, cx| {
            pane.on_spec_seek(*index, window, cx)
        });
        scoresurface::ScoreActions {
            on_rescore: Rc::new(move |window, cx| rescore(&(), window, cx)),
            on_open_folder: Rc::new(move |window, cx| open_folder(&(), window, cx)),
            on_apply_spec: Rc::new(move |window, cx| apply(&(), window, cx)),
            on_revert_spec: Rc::new(move |window, cx| revert(&(), window, cx)),
            on_spec_key: Rc::new(move |event, window, cx| spec_key(event, window, cx)),
            on_spec_seek: Rc::new(move |index, window, cx| spec_seek(&index, window, cx)),
        }
    }

    /// The spec editor's keystrokes: typing edits, the arrows walk the
    /// caret, Ctrl-V pastes at it, and escape reverts the field to the
    /// file's text — the field is never silently cleared.
    fn on_spec_field_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.spec_field.on_key_clipboard(event, cx) {
            FieldAction::Changed | FieldAction::Moved => cx.notify(),
            FieldAction::Escaped => self.revert_spec_field(cx),
            FieldAction::Submitted(_) | FieldAction::None => {}
        }
    }

    /// A click in the spec field: the caret seeks the click and the
    /// field takes the focus.
    fn on_spec_seek(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.spec_field.move_caret_to(index);
        let focus = self.spec_field.focus().clone();
        window.focus(&focus);
        cx.notify();
    }

    /// The field goes back to the file's text — escape and the revert
    /// button both land here.
    fn revert_spec_field(&mut self, cx: &mut Context<Self>) {
        let text = self
            .score_view
            .as_ref()
            .map(|view| view.spec_text.clone())
            .unwrap_or_default();
        self.spec_field.set(&text);
        self.spec_field_loaded = text;
        cx.notify();
    }

    /// Applies the field's text as the spec: validated by the same
    /// core parser the agent's rewrite is refused by, saved with the
    /// backup, then the surface reloads its facts. The scores move
    /// when the reader re-scores.
    fn apply_spec_edit(&mut self, cx: &mut Context<Self>) {
        let text = self.spec_field.value().trim().to_string();
        let spec = match yagna_core::fit::parse_spec(&text) {
            Ok(spec) => spec,
            Err(error) => {
                self.score_note = Some(format!("the spec is refused: {error}"));
                cx.notify();
                return;
            }
        };
        let path = match crate::fitspec::spec_path() {
            Ok(path) => path,
            Err(problem) => {
                self.score_note = Some(problem);
                cx.notify();
                return;
            }
        };
        if let Err(problem) = crate::fitspec::save_to(&path, &spec) {
            self.score_note = Some(problem);
            cx.notify();
            return;
        }
        self.score_note =
            Some("the spec is saved — re-score the catalog to move the scores".to_string());
        self.load_score_cache(cx);
    }

    /// Reads the score surface's facts: the spec, the catalogue, the
    /// two distributions, and what the last re-score did — off the UI
    /// thread.
    fn load_score_cache(&mut self, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let health = store
                .stats_health_buckets()
                .map_err(|error| error.to_string())?;
            let fit = store
                .stats_fit_buckets()
                .map_err(|error| error.to_string())?;
            let rescored_at = store
                .app_state(yagna_store::FIT_RESCORED_AT_KEY)
                .map_err(|error| error.to_string())?;
            let rescored_rows = store
                .app_state(yagna_store::FIT_RESCORED_ROWS_KEY)
                .map_err(|error| error.to_string())?;
            let rescored_moved = store
                .app_state(yagna_store::FIT_RESCORED_MOVED_KEY)
                .map_err(|error| error.to_string())?;
            let (spec, loud_line) = crate::fitspec::load();
            let source = match crate::fitspec::spec_path() {
                Ok(path) if path.exists() => {
                    format!("the spec the file {} carries", path.display())
                }
                _ => "the default spec — no file has been written yet".to_string(),
            };
            let spec_changed =
                crate::fitspec::changed_at().map(|at| at.format("%Y-%m-%d").to_string());
            let catalogue: Vec<(String, String)> = yagna_core::fit::PARAMETERS
                .iter()
                .map(|name| {
                    let kind = match yagna_core::fit::parameter_kind(name) {
                        Some(yagna_core::fit::ParamKind::Number) => "number",
                        Some(yagna_core::fit::ParamKind::Category) => "category",
                        Some(yagna_core::fit::ParamKind::Set) => "set",
                        Some(yagna_core::fit::ParamKind::Boolean) => "boolean",
                        None => "number",
                    };
                    (name.to_string(), kind.to_string())
                })
                .collect();
            let rescored_rows = rescored_rows
                .and_then(|text| text.parse::<u64>().ok())
                .unwrap_or(0);
            let rescored_moved = rescored_moved
                .and_then(|text| text.parse::<u64>().ok())
                .unwrap_or(0);
            Ok::<_, String>(ScoreStats {
                spec,
                loud_line,
                source,
                spec_changed,
                catalogue,
                health,
                fit,
                rescored_at,
                rescored_rows,
                rescored_moved,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_score_stats(result, cx));
        })
        .detach();
    }

    /// Lands the score facts: the surface shows the spec, the two
    /// spreads and the footer — and says what changed since it last
    /// looked.
    fn apply_score_stats(&mut self, result: Result<ScoreStats, String>, cx: &mut Context<Self>) {
        let stats = match result {
            Ok(stats) => stats,
            Err(problem) => {
                self.notice = Some(problem);
                cx.notify();
                return;
            }
        };
        let change = scoresurface::change_line(self.score_previous.as_ref(), &stats.spec);
        let last_rescore = stats
            .rescored_at
            .as_deref()
            .map(|at| (at, stats.rescored_rows, stats.rescored_moved));
        let footer = scoresurface::footer_line(stats.spec_changed.as_deref(), last_rescore);
        let spec_text = serde_json::to_string_pretty(&stats.spec).unwrap_or_default();
        let spec_words = scoresurface::spec_in_words(&stats.spec);
        self.score_previous = Some(stats.spec);
        // The spec field follows the file only while the reader has
        // not typed into it — an in-progress edit survives a reload.
        if spec_field_follows_file(self.spec_field.value(), &self.spec_field_loaded) {
            self.spec_field.set(&spec_text);
            self.spec_field_loaded = spec_text.clone();
        }
        self.score_view = Some(scoresurface::ScoreView {
            spec_text,
            source: stats.source,
            loud_line: stats.loud_line,
            change,
            note: self.score_note.clone(),
            catalogue: stats.catalogue,
            spec_words,
            health: stats.health,
            fit: stats.fit,
            footer,
            busy: self.score_busy,
        });
        cx.notify();
    }

    /// Re-scores the whole catalog from the spec — off the UI thread,
    /// its counts reported on the surface when it lands.
    fn start_rescore(&mut self, cx: &mut Context<Self>) {
        if self.score_busy {
            return;
        }
        self.score_busy = true;
        self.score_note = None;
        cx.notify();
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let (spec, _) = crate::fitspec::load();
            let mut store = lock_catalog(&catalog);
            let now = chrono::Utc::now();
            let report = store
                .rescore_fit(&spec, now)
                .map_err(|error| error.to_string())?;
            let at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            store
                .set_app_state(yagna_store::FIT_RESCORED_AT_KEY, &at)
                .map_err(|error| error.to_string())?;
            store
                .set_app_state(
                    yagna_store::FIT_RESCORED_ROWS_KEY,
                    &report.scored.to_string(),
                )
                .map_err(|error| error.to_string())?;
            store
                .set_app_state(
                    yagna_store::FIT_RESCORED_MOVED_KEY,
                    &report.moved.to_string(),
                )
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(report)
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_rescore(result, cx));
        })
        .detach();
    }

    /// Lands the re-score: the surface says what it covered and how
    /// many rows moved, and the facts reload behind it.
    fn apply_rescore(
        &mut self,
        result: Result<yagna_store::RescoreReport, String>,
        cx: &mut Context<Self>,
    ) {
        self.score_busy = false;
        self.score_note = Some(match result {
            Ok(report) => format!(
                "{} repositories re-scored, {} moved",
                report.scored, report.moved
            ),
            Err(problem) => format!("the re-score failed: {problem}"),
        });
        self.load_score_cache(cx);
        cx.notify();
    }

    /// The spec's folder opens in the platform's file browser.
    fn open_data_folder(&mut self, cx: &mut Context<Self>) {
        let Ok(dir) = crate::paths::data_dir() else {
            self.error = Some("the data directory is unavailable".to_string());
            cx.notify();
            return;
        };
        if let Err(problem) = platform::open_folder(&dir) {
            self.error = Some(problem);
        }
        cx.notify();
    }

    /// Keeps the agent presence fresh: the heartbeat is written while
    /// a session is connected, and the status line reads its age — once
    /// at launch and then once a minute.
    fn watch_agent_presence(&mut self, cx: &mut Context<Self>) {
        self.load_activity(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                let alive = this.update(cx, |pane, cx| pane.load_activity(cx)).is_ok();
                if !alive {
                    return;
                }
            }
        })
        .detach();
    }

    /// Reads the agent's activity trace: the presence line for the
    /// status row and the drawer's rows — off the UI thread.
    fn load_activity(&mut self, cx: &mut Context<Self>) {
        let fetch = cx.background_executor().spawn(async move {
            let path = crate::paths::data_dir()
                .map(|dir| dir.join(crate::mcp::trace::TRACE_FILE))
                .unwrap_or_default();
            crate::mcp::trace::activity_summary(&path, chrono::Utc::now())
        });
        cx.spawn(async move |this, cx| {
            let (presence, rows) = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_activity(presence, rows, cx));
        })
        .detach();
    }

    /// Lands the activity read: the status line and the drawer say what
    /// the agent has been doing.
    fn apply_activity(
        &mut self,
        presence: Option<String>,
        rows: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.agent_line = presence.clone();
        self.sidebar.set_activity(presence, rows);
        cx.notify();
    }

    /// The activity drawer's body: the presence line first, then the
    /// newest actions, newest first.
    fn render_activity_entries(&mut self, column: gpui::Div, _cx: &mut Context<Self>) -> gpui::Div {
        let mut column = column;
        if let Some(presence) = self.sidebar.presence() {
            column = column.child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::accent())
                    .child(SharedString::from(presence.to_string())),
            );
        }
        let word = self.sidebar.filter_text().to_lowercase();
        let rows: Vec<gpui::AnyElement> = self
            .sidebar
            .activity()
            .iter()
            .filter(|row| word.is_empty() || row.to_lowercase().contains(&word))
            .map(|row| {
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(row.clone()))
                    .into_any_element()
            })
            .collect();
        column.children(rows)
    }

    /// A noted subject's drawer entry was clicked: its page opens — the
    /// catalog's row, or the worktree of a local entry.
    fn open_noted(&mut self, subject: NoteSubject, cx: &mut Context<Self>) {
        match subject {
            NoteSubject::Repo(rowid) => {
                let Some(note) = self
                    .sidebar
                    .notes()
                    .iter()
                    .find(|note| note.subject == subject)
                    .cloned()
                else {
                    return;
                };
                let Some(repo) = note.repo_ref.clone() else {
                    return;
                };
                self.open_tab_at(repo, rowid, false, cx);
            }
            NoteSubject::Local(id) => self.open_local_repo(id, cx),
        }
    }

    /// The number of repositories a tag carries, as the sidebar's list
    /// knows it.
    fn tag_count(&self, tag_id: i64) -> u64 {
        self.sidebar
            .tags()
            .iter()
            .find(|tag| tag.id == tag_id)
            .map(|tag| tag.repos)
            .unwrap_or(0)
    }

    /// The name of a tag, as the sidebar's list knows it.
    fn tag_name(&self, tag_id: i64) -> Option<String> {
        self.sidebar
            .tags()
            .iter()
            .find(|tag| tag.id == tag_id)
            .map(|tag| tag.name.clone())
    }

    /// Removes a tag from a row, by id.
    fn unassign_tag(&mut self, repo_rowid: i64, tag_id: i64, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.unassign_tag(repo_rowid as u64, tag_id)?;
            let row = store.search_index_row(repo_rowid)?;
            Ok::<_, StoreError>((repo_rowid, row))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |pane, cx| pane.apply_tag(result, cx));
        })
        .detach();
    }

    /// Deletes a tag and every attachment to it.
    fn delete_tag(&mut self, tag_id: i64, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            let count = store.delete_tag(tag_id)?;
            Ok::<_, StoreError>(count)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let notice = result
                .map(|count| format!("the tag was deleted — {count} repositories lost it"))
                .map_err(|error| format!("the tag could not be deleted: {error}"));
            let _ = this.update(cx, |pane, cx| pane.apply_tag_change(notice, cx));
        })
        .detach();
    }

    /// Merges one tag into another: the attachments move, the source
    /// goes away.
    fn merge_tags(
        &mut self,
        source: i64,
        target: i64,
        count: u64,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let catalog = Arc::clone(&self.catalog);
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.merge_tags(source, target)?;
            Ok::<_, StoreError>(())
        });
        let name = name.to_string();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let notice = result
                .map(|_| format!("'{name}' was merged in — {count} repositories moved"))
                .map_err(|error| format!("the tags could not be merged: {error}"));
            let _ = this.update(cx, |pane, cx| pane.apply_tag_change(notice, cx));
        })
        .detach();
    }

    /// Renames a tag; every attachment follows.
    fn rename_tag(&mut self, tag_id: i64, name: &str, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let name = name.to_string();
        let work = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&catalog);
            store.rename_tag(tag_id, &name)?;
            Ok::<_, StoreError>(())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let notice = result
                .map(|_| "the tag was renamed".to_string())
                .map_err(|error| format!("the tag could not be renamed: {error}"));
            let _ = this.update(cx, |pane, cx| pane.apply_tag_change(notice, cx));
        })
        .detach();
    }

    /// Lands a tag change: the lists and the page are refreshed, and
    /// the sidebar says what happened.
    fn apply_tag_change(&mut self, notice: Result<String, String>, cx: &mut Context<Self>) {
        let notice = match notice {
            Ok(notice) => notice,
            Err(problem) => problem,
        };
        self.sidebar.clear_arms(&notice);
        self.refresh_sidebar(cx);
        self.request_page(cx);
    }

    // --- The paging and the sorting.

    /// Flips to another page and fetches it.
    fn go_to_page(&mut self, page: u32, cx: &mut Context<Self>) {
        let clamped = page.clamp(1, self.page_count());
        if clamped == self.page {
            cx.notify();
            return;
        }
        self.page = clamped;
        self.focused_row = 0;
        self.request_page(cx);
    }

    /// Changes the sort key and re-fetches page 1 under it.
    fn set_sort(&mut self, sort: SortKey, cx: &mut Context<Self>) {
        self.dropdown = None;
        if self.sort == sort {
            cx.notify();
            return;
        }
        self.sort = sort;
        self.page = 1;
        self.request_page(cx);
    }

    // --- The one dropdown.

    /// The dropdown component's closures: a trigger opens, an option is
    /// picked, a keystroke reaches the open list's keyboard map.
    fn dropdown_actions(&self, cx: &mut Context<Self>) -> DropdownActions {
        let on_open = cx.listener(|pane: &mut Self, site: &dropdown::Site, window, cx| {
            pane.toggle_dropdown(*site, window, cx)
        });
        let on_pick = cx.listener(
            |pane: &mut Self, (site, index): &(dropdown::Site, usize), _, cx| {
                pane.pick_dropdown(*site, *index, cx)
            },
        );
        let on_key = cx.listener(
            |pane: &mut Self, (site, event): &(dropdown::Site, KeyDownEvent), _, cx| {
                pane.on_dropdown_key(*site, event, cx)
            },
        );
        DropdownActions {
            on_open: Rc::new(move |site, window, cx| on_open(&site, window, cx)),
            on_pick: Rc::new(move |site, index, window, cx| on_pick(&(site, index), window, cx)),
            on_key: Rc::new(move |site, event: &KeyDownEvent, window, cx| {
                on_key(&(site, event.clone()), window, cx)
            }),
        }
    }

    /// Opens a dropdown — or closes it when it is the open one. One
    /// open dropdown at a time: opening one closes whatever stood
    /// open, and the keyboard starts on the chosen option.
    fn toggle_dropdown(
        &mut self,
        site: dropdown::Site,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dropdown = if self.dropdown == Some(site) {
            None
        } else {
            Some(site)
        };
        self.dropdown_highlight = self.selected_index_of(site);
        if let Some(focus) = self.dropdown_focus(site) {
            let focus = focus.clone();
            window.focus(&focus);
        }
        cx.notify();
    }

    /// A pick writes the choice and closes the list.
    fn pick_dropdown(&mut self, site: dropdown::Site, index: usize, cx: &mut Context<Self>) {
        self.dropdown = None;
        match site {
            dropdown::Site::LibrarySort => {
                if let Some(key) = SortKey::ALL.get(index) {
                    let key = *key;
                    self.set_sort(key, cx);
                }
            }
            dropdown::Site::DiscoverSort => {
                if let Some(sort) = discover::sort_at(index) {
                    self.discover.set_sort(sort);
                }
            }
            dropdown::Site::BuilderField => {
                if let Some(field) = builder::DraftField::at(index) {
                    self.builder.set_draft_field(field);
                }
            }
            dropdown::Site::BuilderOp => {
                let field = self.builder.draft_field();
                if let Some(op) = field.op_at(index) {
                    self.builder.set_draft_op(op);
                }
            }
        }
        cx.notify();
    }

    /// The open list's keyboard map: the arrows walk the options,
    /// `enter` picks, `escape` closes. Keystrokes only reach the map
    /// while this picker's list is the open one.
    fn on_dropdown_key(
        &mut self,
        site: dropdown::Site,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        if self.dropdown != Some(site) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let count = self.dropdown_options(site).len();
        match key {
            "escape" => self.dropdown = None,
            "down" => {
                self.dropdown_highlight =
                    (self.dropdown_highlight + 1).min(count.saturating_sub(1));
            }
            "up" => {
                self.dropdown_highlight = self.dropdown_highlight.saturating_sub(1);
            }
            "enter" => {
                let index = self.dropdown_highlight;
                self.pick_dropdown(site, index, cx);
                return;
            }
            _ => {}
        }
        cx.notify();
    }

    /// The options an open dropdown holds — one list per picker, built
    /// from the state the picker edits.
    fn dropdown_options(&self, site: dropdown::Site) -> Vec<dropdown::DropdownOption> {
        match site {
            dropdown::Site::LibrarySort => SortKey::ALL
                .iter()
                .map(|key| DropdownOption::new(key.label(), *key == self.sort))
                .collect(),
            dropdown::Site::DiscoverSort => discover::sort_options(&self.discover),
            dropdown::Site::BuilderField => builder::DraftField::ALL
                .iter()
                .map(|field| {
                    DropdownOption::new(field.label(), *field == self.builder.draft_field())
                })
                .collect(),
            dropdown::Site::BuilderOp => self
                .builder
                .draft_operators()
                .iter()
                .map(|op| {
                    DropdownOption::new(
                        builder::op_label(*op),
                        Some(*op) == self.builder.draft_op(),
                    )
                })
                .collect(),
        }
    }

    /// Where the keyboard starts on a freshly opened list: on the
    /// chosen option, so `enter` keeps what is there.
    fn selected_index_of(&self, site: dropdown::Site) -> usize {
        self.dropdown_options(site)
            .iter()
            .position(|option| option.selected)
            .unwrap_or(0)
    }

    /// The picker's trigger focus — where the keyboard map lives.
    fn dropdown_focus(&self, site: dropdown::Site) -> Option<&FocusHandle> {
        match site {
            dropdown::Site::LibrarySort => Some(&self.sort_focus),
            dropdown::Site::DiscoverSort => Some(&self.discover_sort_focus),
            dropdown::Site::BuilderField => Some(&self.builder_field_pick_focus),
            dropdown::Site::BuilderOp => Some(&self.builder_op_pick_focus),
        }
    }

    // --- The tabs.

    /// Opens a repository's tab and focuses it. A repository that is
    /// already open is not opened twice — its tab is focused.
    fn open_tab(&mut self, rowid: i64, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.repo_rowid == rowid) else {
            return;
        };
        self.open_tab_at(row.repo_ref.clone(), rowid, false, cx);
    }

    /// Opens a tab from any hit's identity — the deep search's way in,
    /// where the hit may sit outside the current page. A digest hit
    /// opens with its section showing.
    fn open_tab_at(
        &mut self,
        repo: yagna_core::model::RepoRef,
        rowid: i64,
        at_digest: bool,
        cx: &mut Context<Self>,
    ) {
        let index = self.tabs.open(repo, rowid);
        if at_digest && let Some(tab) = self.tabs.tab_mut(index) {
            tab.digest_open = true;
        }
        self.load_tab(rowid, cx);
        cx.notify();
    }

    /// Opens the focused row's tab — the keyboard's way in.
    fn open_focused_tab(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.focused_row) else {
            return;
        };
        let rowid = row.repo_rowid;
        self.open_tab(rowid, cx);
    }

    /// Puts a permanent surface in front: the library, the discovery
    /// surface, the people surface, or the reader's own numbers.
    fn focus_surface(&mut self, which: tabs::Surface, cx: &mut Context<Self>) {
        self.tabs.focus_surface(which);
        match which {
            tabs::Surface::Discover => {
                // The controls show the query the surface holds: the
                // fields are primed before the surface is painted.
                self.prime_discover_controls();
                self.load_discover_cache(cx);
            }
            tabs::Surface::People => self.load_people_cache(cx),
            tabs::Surface::User => self.load_user_stats(cx),
            tabs::Surface::Local => self.load_local_list(cx),
            tabs::Surface::Score => self.load_score_cache(cx),
            tabs::Surface::Library => {}
        }
        cx.notify();
    }

    /// Closes the focused tab.
    fn close_tab(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        self.close_tab_at(index, cx);
    }

    /// Closes the tab at the index.
    fn close_tab_at(&mut self, index: usize, cx: &mut Context<Self>) {
        self.tabs.close(index);
        cx.notify();
    }

    /// Focuses the tab at the index and reads it again — a tab shows
    /// what the store holds now, not what it held when it opened.
    fn focus_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.tabs.focus(index);
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            self.load_local_tab(index, cx);
        } else {
            self.load_tab(tab.rowid, cx);
        }
        cx.notify();
    }

    /// Reads the tab's content from the store: the detail record, the
    /// bodies, the note, the digest, and the assets. One background
    /// task, one lock.
    fn load_tab(&mut self, rowid: i64, cx: &mut Context<Self>) {
        let catalog = Arc::clone(&self.catalog);
        let fetch = cx.background_executor().spawn(async move {
            let store = lock_catalog(&catalog);
            let rowid = rowid as u64;
            let detail = store.repo_detail(rowid)?;
            let readme = store.readme_body(rowid)?;
            let tree = store.tree_body(rowid)?;
            let license = store.license_body(rowid)?;
            let note = store.note_of(NoteSubject::Repo(rowid as i64))?;
            let digest = store.digest_body(rowid)?;
            let assets = store.repo_assets(rowid)?;
            let assets = assets::resolve_assets(&assets);
            let tags: Vec<String> = store
                .repo_tags(rowid)?
                .into_iter()
                .map(|tag| tag.name)
                .collect();
            let blocks = match readme.as_ref() {
                Some(readme) => {
                    let resolver = readme::TabResolver::new(&assets);
                    yagna_core::mdrender::render(&readme.rewritten, &resolver)
                }
                None => Vec::new(),
            };
            let tree_nodes = match tree.as_ref() {
                Some(tree) => yagna_core::treefmt::parse(&tree.rendered),
                None => Vec::new(),
            };
            Ok::<_, StoreError>(TabData {
                detail,
                readme,
                readme_note: None,
                tree,
                tree_nodes,
                license,
                note,
                digest,
                assets,
                blocks,
                tags,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |pane, cx| pane.apply_tab_data(rowid, result, cx));
        })
        .detach();
    }

    /// Lands the tab's content: the first open starts the images, asks
    /// for the first commit, and asks the forge for the README and the
    /// non-standard license text the store is missing; every open reads
    /// the tab again.
    fn apply_tab_data(
        &mut self,
        rowid: i64,
        result: Result<TabData, StoreError>,
        cx: &mut Context<Self>,
    ) {
        let Ok(mut data) = result else {
            self.notice = result
                .err()
                .map(|error| format!("the tab could not be read — {error}"));
            cx.notify();
            return;
        };
        let Some(index) = self.tabs.index_of(rowid) else {
            return;
        };
        let first_open = self
            .tabs
            .tab(index)
            .map(|tab| !tab.images_started)
            .unwrap_or(false);
        // Why the README is missing, when its fetch failed — the tab
        // keeps the answer so a re-read does not lose it.
        if let Some(tab) = self.tabs.tab_mut(index) {
            data.readme_note = tab.readme_error.clone();
            tab.data = Some(data);
        }
        if first_open {
            self.mark_images_started(index);
            self.start_images(rowid, cx);
            self.maybe_ask_tab_readme(index, cx);
            self.maybe_ask_tab_license(index, cx);
        }
        self.maybe_first_commit(index, cx);
        cx.notify();
    }

    /// Asks for the focused tab's missing README, on demand — once per
    /// tab. A catalog repository only: a shadow tab fetches its README
    /// live when it opens.
    fn maybe_ask_tab_readme(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.readme_asked || tab.rowid < 0 || tab.is_local() {
            return;
        }
        let missing = tab.data.as_ref().is_some_and(|data| data.readme.is_none());
        if !missing {
            return;
        }
        let repo = tab.repo.clone();
        let rowid = tab.rowid;
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.readme_asked = true;
            tab.readme_fetching = true;
        }
        self.fetch_tab_readme(rowid, repo, cx);
    }

    /// The tab's README fetch, off the UI thread — the same worker the
    /// expanded row uses, one writer, one queue row.
    fn fetch_tab_readme(
        &mut self,
        rowid: i64,
        repo: yagna_core::model::RepoRef,
        cx: &mut Context<Self>,
    ) {
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let task = cx.background_executor().spawn(async move {
            workers::fetch_readme(&catalog, &services, rowid as u64, &repo).await
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_tab_readme(rowid, outcome, cx));
        })
        .detach();
    }

    /// Lands the tab's README: the tab reads itself again, or says why
    /// in one line and leaves the rest of the tab standing.
    fn apply_tab_readme(
        &mut self,
        rowid: i64,
        outcome: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.index_of(rowid) else {
            return;
        };
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.readme_fetching = false;
        }
        match outcome {
            Ok(()) => self.load_tab(rowid, cx),
            Err(why) => {
                if let Some(tab) = self.tabs.tab_mut(index) {
                    tab.readme_error = Some(why);
                }
                if let Some(tab) = self.tabs.tab_mut(index)
                    && let Some(data) = tab.data.as_mut()
                {
                    data.readme_note = tab.readme_error.clone();
                }
                cx.notify();
            }
        }
    }

    /// Asks for the focused tab's non-standard license text, on demand
    /// — once per tab. A standard licence is a name, not a file: it is
    /// a fact, and no text is fetched for it.
    fn maybe_ask_tab_license(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.license_asked || tab.rowid < 0 || tab.is_local() {
            return;
        }
        let nonstandard = tab
            .data
            .as_ref()
            .and_then(|data| data.detail.as_ref())
            .and_then(|detail| detail.license.as_ref())
            .and_then(|license| license.spdx_id.clone())
            .is_none_or(|id| id == "NOASSERTION");
        let missing = tab.data.as_ref().is_some_and(|data| data.license.is_none());
        if !nonstandard || !missing {
            return;
        }
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.license_asked = true;
        }
        self.load_license(cx);
    }

    /// Marks that the tab's images have been asked for.
    fn mark_images_started(&mut self, index: usize) {
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.images_started = true;
        }
    }

    /// Starts the image runner: the repository's missing images are
    /// enqueued and four workers work the queue. The queue paces, not
    /// the tab; the image policy decides whether a tab open fetches.
    fn start_images(&mut self, rowid: i64, cx: &mut Context<Self>) {
        if !self.settings.image_policy.tab_may_fetch() {
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let enqueue = Arc::clone(&self.catalog);
        let row_key = rowid as u64;
        let enqueue = cx.background_executor().spawn(async move {
            let mut store = lock_catalog(&enqueue);
            store.enqueue_repo_assets(row_key, Utc::now()).unwrap_or(0)
        });
        cx.spawn(async move |this, cx| {
            let count = enqueue.await;
            let _ = this.update(cx, |pane, cx| {
                if count > 0 {
                    pane.notice = Some(format!("{count} image(s) are on their way"));
                }
                cx.notify();
            });
        })
        .detach();
        let _ = &catalog;
        for _ in 0..IMAGE_CONCURRENCY {
            let catalog = Arc::clone(&self.catalog);
            let services = Arc::clone(&self.services);
            let cache_dir = self.cache_dir.clone();
            let task = cx.background_executor().spawn(async move {
                workers::run_images(&catalog, &services, &cache_dir, rowid as u64).await
            });
            cx.spawn(async move |this, cx| {
                let cached = task.await;
                let _ = this.update(cx, |pane, cx| pane.apply_images(rowid, cached, cx));
            })
            .detach();
        }
    }

    /// Lands the image runner's work: the tab reads its assets again.
    fn apply_images(&mut self, rowid: i64, cached: u64, cx: &mut Context<Self>) {
        if cached == 0 {
            return;
        }
        self.load_tab(rowid, cx);
    }

    /// Asks for the repository's exact first commit — once, and only
    /// when the tab may: the setting allows it, and the stored date is
    /// still the creation's.
    fn maybe_first_commit(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.settings.exact_first_commit {
            return;
        }
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.first_commit_asked {
            return;
        }
        let source = tab
            .data
            .as_ref()
            .and_then(|data| data.detail.as_ref())
            .and_then(|detail| detail.first_commit_source.clone());
        if source.as_deref() != Some(FIRST_COMMIT_SOURCE_DEFAULT) {
            return;
        }
        let repo = tab.repo.clone();
        let rowid = tab.rowid;
        if let Some(tab) = self.tabs.tab_mut(index) {
            tab.first_commit_asked = true;
        }
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let task = cx.background_executor().spawn(async move {
            workers::fetch_first_commit(&catalog, &services, rowid as u64, &repo).await
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_first_commit(rowid, outcome, cx));
        })
        .detach();
    }

    /// Lands the first commit's date: the tab reads itself again.
    fn apply_first_commit(
        &mut self,
        rowid: i64,
        outcome: Result<bool, String>,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(true) => self.load_tab(rowid, cx),
            Ok(false) => {}
            Err(problem) => self.notice = Some(problem),
        }
        cx.notify();
    }

    /// The tab's actions: what its interactive parts report.
    fn tab_actions(&self, cx: &mut Context<Self>) -> TabViewActions {
        let external = cx.listener(|pane: &mut Self, url: &str, _, cx| pane.open_external(url, cx));
        let license =
            cx.listener(|pane: &mut Self, open: &bool, _, cx| pane.toggle_license(*open, cx));
        let structure = cx.listener(|pane: &mut Self, _: &(), _, cx| pane.load_structure(cx));
        let directory = cx.listener(|pane: &mut Self, path: &str, _, cx| pane.toggle_dir(path, cx));
        let drawer = cx.listener(|pane: &mut Self, _, _, cx| pane.toggle_structure(cx));
        let copy = cx.listener(|pane: &mut Self, _, _, cx| pane.copy_readme(cx));
        let license_text = cx.listener(|pane: &mut Self, _, _, cx| pane.load_license(cx));
        let edit_tags = cx.listener(|pane: &mut Self, _, window, cx| {
            cx.stop_propagation();
            pane.edit_focused_tags(cx);
            let _ = window;
        });
        let edit_note = cx.listener(|pane: &mut Self, _, window, cx| {
            pane.edit_note(window, cx);
        });
        let prefetch_tab = cx.listener(|pane: &mut Self, _, _, cx| pane.prefetch_tab(cx));
        let generate_digest = cx.listener(|pane: &mut Self, _, _, cx| pane.generate_digest(cx));
        let toggle_digest = cx.listener(|pane: &mut Self, _, _, cx| pane.toggle_digest(cx));
        let copy_digest = cx.listener(|pane: &mut Self, _, _, cx| pane.copy_digest(cx));
        let save_digest = cx.listener(|pane: &mut Self, _, _, cx| pane.save_digest(cx));
        TabViewActions {
            on_external: Rc::new(external),
            on_license: Rc::new(move |open: bool, window: &mut Window, cx: &mut App| {
                license(&open, window, cx)
            }),
            on_load_structure: Rc::new(move |window: &mut Window, cx: &mut App| {
                structure(&(), window, cx)
            }),
            on_toggle_dir: Rc::new(directory),
            on_toggle_structure: Rc::new(move |window: &mut Window, cx: &mut App| {
                drawer(&(), window, cx)
            }),
            on_copy: Rc::new(move |window: &mut Window, cx: &mut App| copy(&(), window, cx)),
            on_load_license: Rc::new(move |window: &mut Window, cx: &mut App| {
                license_text(&(), window, cx)
            }),
            on_edit_tags: Rc::new(move |window: &mut Window, cx: &mut App| {
                edit_tags(&(), window, cx)
            }),
            on_edit_note: Rc::new(move |window: &mut Window, cx: &mut App| {
                edit_note(&(), window, cx)
            }),
            on_prefetch: Rc::new(move |window: &mut Window, cx: &mut App| {
                prefetch_tab(&(), window, cx)
            }),
            on_generate_digest: Rc::new(move |window: &mut Window, cx: &mut App| {
                generate_digest(&(), window, cx)
            }),
            on_toggle_digest: Rc::new(move |window: &mut Window, cx: &mut App| {
                toggle_digest(&(), window, cx)
            }),
            on_copy_digest: Rc::new(move |window: &mut Window, cx: &mut App| {
                copy_digest(&(), window, cx)
            }),
            on_save_digest: Rc::new(move |window: &mut Window, cx: &mut App| {
                save_digest(&(), window, cx)
            }),
        }
    }

    /// Opens the sidebar's tag editor for the focused repository — the
    /// tab's own "tags" button.
    fn edit_focused_tags(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            return;
        }
        self.sidebar.edit(tab.rowid);
        self.refresh_editor_tags(cx);
        self.tag_field.clear();
        cx.notify();
    }

    /// Fetches the licence text of the focused repository — the tab's
    /// "fetch the license text" action.
    fn load_license(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            return;
        }
        let repo = tab.repo.clone();
        let rowid = tab.rowid as u64;
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let task = cx
            .background_executor()
            .spawn(async move { workers::fetch_license(&catalog, &services, rowid, &repo).await });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_license(rowid, outcome, cx));
        })
        .detach();
    }

    /// Lands the licence text: the tab reads itself again.
    fn apply_license(&mut self, rowid: u64, outcome: Result<(), String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(()) => self.load_tab(rowid as i64, cx),
            Err(problem) => self.notice = Some(problem),
        }
        cx.notify();
    }

    /// Copies the tab's README to the clipboard — the pinned framework
    /// cannot select text, so this is how a reader takes the body away.
    fn copy_readme(&mut self, cx: &mut Context<Self>) {
        let body = self
            .tabs
            .focused()
            .and_then(|tab| tab.data.as_ref())
            .and_then(|data| data.readme.as_ref())
            .map(|readme| readme.rewritten.clone());
        let Some(body) = body else {
            self.notice = Some("there is no readme to copy".to_string());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(body));
        self.notice = Some("the readme is on the clipboard".to_string());
        cx.notify();
    }

    /// Opens a URL outside the app: the tab's "open on GitHub" action,
    /// and the reader's links. The status line says what was opened, so
    /// a reader can see that the click landed.
    fn open_external(&mut self, url: &str, cx: &mut Context<Self>) {
        match platform::open_external(url) {
            Ok(()) => self.notice = Some(format!("opening {url}")),
            Err(problem) => self.error = Some(problem),
        }
        cx.notify();
    }

    /// Pulls a scope into the cache, in the background: the bodies,
    /// the licences, the trees and the images, all minimised.
    fn prefetch(&mut self, scope: PrefetchScope, plan: PrefetchPlan, cx: &mut Context<Self>) {
        let Some(token) = self.services.token().map(|token| token.to_string()) else {
            self.notice = Some("no token — set GITHUB_TOKEN to pre-fetch".to_string());
            cx.notify();
            return;
        };
        let Some(host) = self.services.host.clone() else {
            self.notice = Some("the runtime is not there — nothing can be pre-fetched".to_string());
            cx.notify();
            return;
        };
        let catalog = Arc::clone(&self.catalog);
        std::thread::spawn(move || {
            crate::prefetch::run_with(
                &catalog,
                &host,
                &token,
                &crate::prefetch::Options { scope, plan },
            );
        });
        self.notice = Some("the pre-fetch is running in the background".to_string());
        cx.notify();
    }

    /// Pulls everything of the focused repository into the cache.
    fn prefetch_tab(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            return;
        }
        let scope = PrefetchScope::Repository(tab.repo.owner.clone(), tab.repo.name.clone());
        let plan = PrefetchPlan {
            images: ImagePolicy::All,
            licenses: true,
            trees: true,
        };
        self.prefetch(scope, plan, cx);
    }

    /// Pulls the filtered list into the cache.
    fn prefetch_filtered(&mut self, cx: &mut Context<Self>) {
        let tag_ids = self.sidebar.filter().tag_ids;
        if tag_ids.is_empty() {
            self.notice = Some("no filter is on — nothing to pre-fetch".to_string());
            cx.notify();
            return;
        }
        let scope = PrefetchScope::TagIds(tag_ids);
        let plan = PrefetchPlan {
            images: ImagePolicy::FirstBytes(2 * 1024 * 1024),
            licenses: true,
            trees: false,
        };
        self.prefetch(scope, plan, cx);
    }

    /// Opens or closes the license text.
    fn toggle_license(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.focused_mut() {
            tab.license_open = open;
        }
        cx.notify();
    }

    /// Opens or closes the structure drawer.
    fn toggle_structure(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.focused_mut() {
            tab.structure_open = !tab.structure_open;
        }
        cx.notify();
    }

    /// Folds a directory away or opens it.
    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.focused_mut() {
            if tab.collapsed.contains(path) {
                tab.collapsed.remove(path);
            } else {
                tab.collapsed.insert(path.to_string());
            }
        }
        cx.notify();
    }

    /// Requests the structure: the tab's "load structure" action.
    fn load_structure(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            return;
        }
        let repo = tab.repo.clone();
        let rowid = tab.rowid;
        let branch = tab
            .data
            .as_ref()
            .and_then(|data| data.detail.as_ref())
            .and_then(|detail| detail.default_branch.clone());
        let Some(branch) = branch else {
            self.error = Some("this repository has no default branch".to_string());
            cx.notify();
            return;
        };
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        let task = cx.background_executor().spawn(async move {
            workers::fetch_structure(&catalog, &services, rowid as u64, &repo, &branch).await
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_structure(rowid, outcome, cx));
        })
        .detach();
    }

    /// Lands the structure: the tab reads itself again.
    fn apply_structure(&mut self, rowid: i64, outcome: Result<(), String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(()) => self.load_tab(rowid, cx),
            Err(problem) => self.notice = Some(problem),
        }
        cx.notify();
    }

    /// Takes the digest of the focused repository — the tab's Generate
    /// (or regenerate) action. The pipeline runs off the UI thread;
    /// when it lands, the tab reads itself again.
    fn generate_digest(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.focused_index() else {
            return;
        };
        let Some(tab) = self.tabs.tab(index) else {
            return;
        };
        if tab.is_local() {
            return;
        }
        let rowid = tab.rowid as u64;
        let catalog = Arc::clone(&self.catalog);
        let services = Arc::clone(&self.services);
        self.notice = Some("the digest is being taken".to_string());
        let task = cx
            .background_executor()
            .spawn(async move { workers::fetch_digest(&catalog, &services, rowid).await });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_digest(rowid as i64, outcome, cx));
        })
        .detach();
    }

    /// Lands the digest: the tab reads itself again, or the status line
    /// says what went wrong.
    fn apply_digest(&mut self, rowid: i64, outcome: Result<(), String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(()) => self.load_tab(rowid, cx),
            Err(problem) => self.notice = Some(problem),
        }
        cx.notify();
    }

    /// Shows or folds the digest's body.
    fn toggle_digest(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.focused_mut() {
            tab.digest_open = !tab.digest_open;
        }
        cx.notify();
    }

    /// Copies the digest body to the clipboard — how a reader takes the
    /// gitingest into a prompt.
    fn copy_digest(&mut self, cx: &mut Context<Self>) {
        let body = self
            .tabs
            .focused()
            .and_then(|tab| tab.data.as_ref())
            .and_then(|data| data.digest.as_ref())
            .map(|digest| digest.body.clone());
        let Some(body) = body else {
            self.notice = Some("there is no digest to copy".to_string());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(body));
        self.notice = Some("the digest is on the clipboard".to_string());
        cx.notify();
    }

    /// Saves the digest as a `.txt` through the platform seam.
    fn save_digest(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.focused() else {
            return;
        };
        let Some(digest) = tab.data.as_ref().and_then(|data| data.digest.as_ref()) else {
            self.notice = Some("there is no digest to save".to_string());
            cx.notify();
            return;
        };
        match platform::save_digest(&tab.repo.owner, &tab.repo.name, &digest.body) {
            Ok(path) => self.notice = Some(format!("the digest was saved to {}", path.display())),
            Err(problem) => self.error = Some(problem),
        }
        cx.notify();
    }

    // --- The shell chrome.

    /// The window's top row: the brand, the menu, the update's own
    /// slot with its stop control, and the gear that opens the
    /// settings. The settings live here, never inside a tab: opening
    /// them never touches the tab stack.
    ///
    /// The row is the window's last painted child and sits at the top
    /// of the window on its own: that way the menu's dropdown hangs
    /// over the body instead of under it.
    fn render_shell(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let actions = self.menu_actions(cx);
        let status = self.updater.status();
        let mut line = status.line(Instant::now());
        if self.score_busy {
            line = Some("re-scoring the catalog".to_string());
        }
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(chrome_height())
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .child(shell::render_brand())
            .child(div().flex_1())
            .child(menu::render_row(self.menu_head, &actions))
            .when_some(self.agent_line.clone(), |row, line| {
                row.child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::accent())
                        .child(SharedString::from(line)),
                )
            })
            .child(shell::render_update_card(line.as_deref()))
            .when(status.stoppable(), |row| {
                row.child(self.render_update_stop(cx))
            })
            .child(self.render_settings_gear(cx))
    }

    /// The stop control: the sweep's own way out, beside the running
    /// card. It asks the worker to stop at the next page boundary and
    /// says "stopping…" while it does — the card never keeps spinning.
    fn render_update_stop(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let stopping = self.updater.status().is_stopping();
        let label: SharedString = if stopping {
            "stopping…".into()
        } else {
            "stop the update".into()
        };
        let tip = self.tooltip.clone();
        let stop = cx.listener(|pane, _, _, cx| pane.stop_update(cx));
        control::icon_button(
            "update-stop",
            icons::Icon::Close,
            label,
            &tip,
            move |_, window, cx| stop(&ClickEvent::default(), window, cx),
        )
        .text_color(theme::health_low())
    }

    /// The gear: the settings' way in, beside the brand. It is the
    /// only way in — the list header carries no settings link.
    fn render_settings_gear(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("shell-settings")
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .on_click(cx.listener(|pane, _, _, cx| pane.toggle_settings(cx)))
            .child(tabs::icon_element(
                icons::Icon::Settings,
                if self.settings_open {
                    theme::accent()
                } else {
                    theme::text()
                },
            ))
    }

    /// The menu's closures: a head opens or closes, an item asks for a
    /// command.
    fn menu_actions(&self, cx: &mut Context<Self>) -> menu::MenuActions {
        let on_command = cx.listener(|pane: &mut Self, command: &menu::Command, _, cx| {
            pane.apply_menu_command(*command, cx)
        });
        let on_head = cx
            .listener(|pane: &mut Self, head: &menu::Head, _, cx| pane.toggle_menu_head(*head, cx));
        menu::MenuActions {
            on_command: Rc::new(
                move |command: menu::Command, window: &mut Window, cx: &mut App| {
                    on_command(&command, window, cx)
                },
            ),
            on_head: Rc::new(move |head: menu::Head, window: &mut Window, cx: &mut App| {
                on_head(&head, window, cx)
            }),
        }
    }

    /// Opens the head's dropdown, or closes the one that is open.
    fn toggle_menu_head(&mut self, head: menu::Head, cx: &mut Context<Self>) {
        self.menu_head = if self.menu_head == Some(head) {
            None
        } else {
            Some(head)
        };
        cx.notify();
    }

    /// What a menu item asks for. The commands are the pane's own: the
    /// menu names them, the pane does them.
    fn apply_menu_command(&mut self, command: menu::Command, cx: &mut Context<Self>) {
        self.menu_head = None;
        match command {
            menu::Command::UpdateRepositories => self.start_update(cx),
            menu::Command::PrefetchFiltered => self.prefetch_filtered(cx),
            menu::Command::ExportCatalog => self.export_catalog(cx),
            menu::Command::ImportCatalog => self.import_catalog(cx),
            menu::Command::Quit => cx.quit(),
            menu::Command::CycleTheme => self.cycle_theme(cx),
            menu::Command::IncreaseTextSize => self.change_text_size(FONT_SCALE_STEP, cx),
            menu::Command::DecreaseTextSize => self.change_text_size(-FONT_SCALE_STEP, cx),
            menu::Command::ToggleSidebar => self.toggle_sidebar(cx),
            menu::Command::About => self.open_dialog(Dialog::About, cx),
            menu::Command::Shortcuts => self.open_dialog(Dialog::Help, cx),
            menu::Command::ReportIssue => self.report_issue(cx),
        }
        cx.notify();
    }

    /// Moves to the next theme: the picker and the menu share the one
    /// applier.
    fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        let next = next_theme_name(&self.settings.theme);
        self.apply_theme(next, cx);
    }

    /// Moves the type ramp up or down, and remembers where the reader
    /// left it.
    fn change_text_size(&mut self, step: i32, cx: &mut Context<Self>) {
        let current = (typography::scale() * 100.0).round() as i32;
        let target = (current + step).clamp(0, 255) as u8;
        typography::set_scale(target);
        let applied = (typography::scale() * 100.0).round() as u8;
        self.settings.font_scale = applied;
        self.save_settings(cx);
        self.notice = Some(format!("the text is {applied}% of the ramp"));
        cx.notify();
    }

    /// Shows or hides the tag sidebar.
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// Opens the shell's card over the window.
    fn open_dialog(&mut self, which: Dialog, cx: &mut Context<Self>) {
        self.dialog = Some(which);
        cx.notify();
    }

    /// Closes the shell's card.
    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        cx.notify();
    }

    /// Opens the issue tracker in the browser — the app's own seam,
    /// never a command line.
    fn report_issue(&mut self, cx: &mut Context<Self>) {
        self.open_external(ISSUE_TRACKER_URL, cx);
    }

    /// The About card, over the window.
    fn render_about(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let close = cx.listener(|pane: &mut Self, _: &ClickEvent, _, cx| pane.close_dialog(cx));
        let handler: shell::CloseHandler = Rc::new(move |window: &mut Window, cx: &mut App| {
            close(&ClickEvent::default(), window, cx)
        });
        shell::render_about(self.data_dir.as_deref(), &handler)
    }

    /// The shortcuts card, over the window.
    fn render_help(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let close = cx.listener(|pane: &mut Self, _: &ClickEvent, _, cx| pane.close_dialog(cx));
        let handler: shell::CloseHandler = Rc::new(move |window: &mut Window, cx: &mut App| {
            close(&ClickEvent::default(), window, cx)
        });
        shell::render_help(&handler)
    }

    /// The settings' footer: the author's mark under the modal, where
    /// the reader can see who built it.
    fn render_settings_footer(&mut self) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-footer")
            .absolute()
            .bottom_3()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(shell::render_author_mark())
    }

    /// Writes the catalog out, off the UI thread: the export verb's
    /// own export, into the working directory.
    fn export_catalog(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            crate::transfer::run_export(crate::transfer::ExportFormat::Json, None)
        });
        cx.spawn(async move |this, cx| {
            let code = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_export(code, cx));
        })
        .detach();
    }

    /// Lands the export's answer.
    fn apply_export(&mut self, code: std::process::ExitCode, cx: &mut Context<Self>) {
        self.notice = Some(if verb_succeeded(code) {
            "the catalog was exported into the working directory".to_string()
        } else {
            "the export failed — the verb said why on the console".to_string()
        });
        cx.notify();
    }

    /// Merges an exported catalog back in, off the UI thread: the
    /// newest export in the working directory is the one that is read.
    fn import_catalog(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            let path = newest_export()?;
            Some(crate::transfer::run_import(&path))
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |pane, cx| pane.apply_import(outcome, cx));
        })
        .detach();
    }

    /// Lands the import's answer.
    fn apply_import(&mut self, outcome: Option<std::process::ExitCode>, cx: &mut Context<Self>) {
        match outcome {
            None => {
                self.notice =
                    Some("there is no export in the working directory to import".to_string());
            }
            Some(code) if verb_succeeded(code) => {
                self.notice = Some("the catalog was imported".to_string());
                self.request_page(cx);
                self.refresh_sidebar(cx);
            }
            Some(_) => {
                self.notice =
                    Some("the import failed — the verb said why on the console".to_string());
            }
        }
        cx.notify();
    }

    // --- The rendering.

    /// The header: the total count on the left, the sort picker on the
    /// right. The picker is the one dropdown component's header shape:
    /// the trigger in the bar, the open list hanging absolutely under
    /// it (the bar's own row is fixed-height, so the list cannot grow
    /// inside it).
    fn render_header(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let dropdowns = self.dropdown_actions(cx);
        let open = self.dropdown == Some(dropdown::Site::LibrarySort);
        let on_open = Rc::clone(&dropdowns.on_open);
        let on_key = Rc::clone(&dropdowns.on_key);
        let site = dropdown::Site::LibrarySort;
        let focus = self.sort_focus.clone();
        let count = if self.is_searching() {
            format!("matches {}", self.total)
        } else {
            format!("{} repositories", self.total)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(chrome_height())
            .px_3()
            .bg(theme::surface())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(tabs::icon_element(icons::Icon::Star, theme::text_muted()))
                    .child(
                        div()
                            .text_size(RowLayout::small_size())
                            .text_color(theme::text_muted())
                            .child(SharedString::from(count)),
                    ),
            )
            .child(
                div()
                    .id("prefetch-filtered")
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_alt()))
                    .text_size(RowLayout::small_size())
                    .text_color(theme::accent())
                    .when(!self.sidebar.filter().tag_ids.is_empty(), |button| {
                        button.on_click(cx.listener(|pane, _, _, cx| pane.prefetch_filtered(cx)))
                    })
                    .child("pre-fetch the filtered list"),
            )
            .child(self.render_update_button(cx))
            .child(
                div()
                    .id("sort-toggle")
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .track_focus(&focus)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_alt()))
                    .text_size(RowLayout::small_size())
                    .text_color(if open { theme::accent() } else { theme::text() })
                    .on_click(move |_, window, cx| on_open(site, window, cx))
                    .on_key_down(move |event, window, cx| on_key(site, event, window, cx))
                    .child(tabs::icon_element(
                        icons::Icon::ChevronDown,
                        if open { theme::accent() } else { theme::text() },
                    ))
                    .child(SharedString::from(format!(
                        "sorted by {} {}",
                        self.sort.label(),
                        if open { "▴" } else { "▾" }
                    ))),
            )
    }

    /// The UPDATE REPOSITORIES button: the sweep's verb, one at a
    /// time. While the sweep runs the button reads the status — the
    /// progress line and the age of the run — and a finished run keeps
    /// its summary until the next one supersedes it.
    fn render_update_button(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let status = self.updater.status();
        let running = status.is_running();
        let label = status
            .line(Instant::now())
            .unwrap_or_else(|| "update repositories".to_string());
        let ink = if running {
            theme::text_muted()
        } else {
            theme::text()
        };
        div()
            .id("update-repositories")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(control::CONTROL_HEIGHT)
            .px_3()
            .rounded_sm()
            .border_2()
            .border_color(gpui::transparent_black())
            .bg(theme::surface())
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .text_color(ink)
            .on_click(cx.listener(|pane, _, _, cx| pane.start_update(cx)))
            .child(tabs::icon_element(icons::Icon::Refresh, ink))
            .child(SharedString::from(label))
    }

    /// The search row: the field, and the note about the index.
    fn render_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let field = self
            .search_field
            .render(
                window,
                cx.listener(|pane, event, window, cx| pane.on_search_key(event, window, cx)),
                {
                    let focus = self.search_field.focus().clone();
                    move |window, _| window.focus(&focus)
                },
            )
            .w(SEARCH_WIDTH);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .child(field)
            .child(self.render_search_button(cx))
            .child(self.render_deep_button(cx))
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(self.index_note.clone())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .when(self.expanded.open_count() > 0, |note| {
                        note.child(SharedString::from(format!(
                            "{} rows open",
                            self.expanded.open_count()
                        )))
                    }),
            )
    }

    /// The deep search's toggle: the two searches stay deliberately
    /// separate — instant fuzzy over names and tags, deep FTS over
    /// README and digest text.
    fn render_deep_button(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("deep-toggle")
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .text_color(if self.deep_open {
                theme::accent()
            } else {
                theme::text()
            })
            .on_click(cx.listener(|pane, _, _, cx| pane.toggle_deep(cx)))
            .child("deep search")
    }

    /// The deep-search panel: the query field, the syntax's hint, and
    /// the hits — each with its repository, its source, and the
    /// snippet's bracketed match.
    fn render_deep_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let field = self
            .deep_field
            .render(
                window,
                cx.listener(|pane, event, window, cx| pane.on_deep_key(event, window, cx)),
                {
                    let focus = self.deep_field.focus().clone();
                    move |window, _| window.focus(&focus)
                },
            )
            .w(SEARCH_WIDTH);
        // The panel's bar wears its own ground, so the panel reads as a
        // panel over the list — never as more list.
        let mut column = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .bg(theme::surface_alt())
            .border_b_1()
            .border_color(theme::border());
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(field)
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child("enter runs it · down walks the hits · enter opens one"),
                ),
        );
        let actions = self.builder_actions(cx);
        let dropdowns = self.dropdown_actions(cx);
        let pickers = builder::PickerState {
            open: self.dropdown,
            highlight: self.dropdown_highlight,
            dropdown: &dropdowns,
            field_focus: &self.builder_field_pick_focus,
            op_focus: &self.builder_op_pick_focus,
        };
        column = column.child(builder::render(
            &self.builder,
            &mut self.builder_field,
            window,
            &actions,
            &pickers,
        ));
        if let Some(note) = &self.deep_note {
            column = column.child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(note.clone())),
            );
        }
        if self.deep_running {
            column = column.child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child("searching…"),
            );
        }
        let snapshot = self.deep_hits.clone();
        let hits: Vec<gpui::AnyElement> = snapshot
            .iter()
            .enumerate()
            .map(|(index, hit)| self.render_deep_hit(index, hit, cx))
            .collect();
        if !hits.is_empty() {
            column = column.child(
                div()
                    .id("deep-results")
                    .max_h(DEEP_RESULTS_HEIGHT)
                    .overflow_y_scroll()
                    .scrollbar_width(repo::TabLayout::SCROLLBAR)
                    .flex()
                    .flex_col()
                    .children(hits),
            );
        }
        column
    }

    /// One deep hit: the repository's name, where the match came from,
    /// and the snippet. A click opens it, like enter does.
    fn render_deep_hit(
        &mut self,
        index: usize,
        hit: &SearchHit,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = index == self.deep_selected;
        let rowid = hit.row.repo_rowid;
        let at_digest = hit.source == SearchSource::Digest;
        let repo = hit.row.repo_ref.clone();
        div()
            .id(SharedString::from(format!("deep-hit-{index}")))
            .px_2()
            .py_1()
            .rounded_sm()
            .when(selected, |row| row.bg(theme::surface_alt()))
            .hover(|style| style.bg(theme::surface_alt()))
            .cursor_pointer()
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.open_tab_at(repo.clone(), rowid, at_digest, cx)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(RowLayout::small_size())
                            .text_color(if selected {
                                theme::accent()
                            } else {
                                theme::text()
                            })
                            .child(SharedString::from(format!(
                                "{}/{}",
                                hit.row.repo_ref.owner, hit.row.repo_ref.name
                            ))),
                    )
                    .child(
                        div()
                            .text_size(RowLayout::small_size())
                            .text_color(theme::text_muted())
                            .child(hit.source.label()),
                    ),
            )
            .child(
                div()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(hit.snippet.clone())),
            )
            .into_any_element()
    }

    /// The search's button: the reader asks for the search, and the
    /// results arrive when they arrive. Typing alone does not rank the
    /// catalog.
    fn render_search_button(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let label = if self.search.is_active() {
            "search again"
        } else {
            "search"
        };
        div()
            .id("search-button")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(control::CONTROL_HEIGHT)
            .px_3()
            .rounded_sm()
            .bg(theme::accent())
            .cursor_pointer()
            .hover(|style| style.opacity(0.9))
            .text_size(RowLayout::small_size())
            .text_color(theme::background())
            .on_click(cx.listener(|pane, _, _, cx| pane.run_query(cx)))
            .child(tabs::icon_element(icons::Icon::Search, theme::background()))
            .child(label)
    }

    /// The library sort's open list: the same dropdown list, anchored
    /// under the library header — the third chrome row — on its own
    /// ground, over the rows.
    fn render_sort_popup(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let dropdowns = self.dropdown_actions(cx);
        let options = self.dropdown_options(dropdown::Site::LibrarySort);
        let highlight = self.dropdown_highlight;
        div()
            .absolute()
            .top(chrome_height() * 3.0)
            .right_3()
            .child(dropdown::render_popup(
                dropdown::Site::LibrarySort,
                highlight,
                &options,
                &dropdowns,
            ))
    }

    /// The list itself: the variable-height `list` over the current
    /// page — the virtualization owns the scroll; the renderer produces
    /// only the visible rows, reusing the cached view models.
    fn render_list(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let state = self.list_state.clone();
        div().flex_1().min_h_0().w_full().child(
            div().w_full().h_full().max_w(LIST_WIDTH).child(
                list(
                    state,
                    cx.processor(|pane, index, window, cx| pane.render_item(index, window, cx)),
                )
                .h_full(),
            ),
        )
    }

    /// One row of the list.
    fn render_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display = self.display_for(index, window);
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let repo_rowid = row.repo_rowid;
        let chevron = Chevron {
            open: self.expanded.is_open(repo_rowid),
            generation: self.expanded.generation(repo_rowid),
            rowid: repo_rowid,
        };
        let open = self.expanded.is_open(repo_rowid);
        let preview = if open {
            self.expanded.preview(repo_rowid)
        } else {
            None
        };
        let pending = open && self.expanded.is_pending(repo_rowid);
        let actions = RowActions {
            on_chevron: Rc::new(cx.listener(move |pane: &mut Self, _: &ClickEvent, _, cx| {
                pane.toggle_expanded(index, cx)
            })),
            on_open: Rc::new(cx.listener(move |pane: &mut Self, _: &ClickEvent, _, cx| {
                pane.open_tab(repo_rowid, cx)
            })),
            on_remove_tag: Rc::new(
                cx.listener(|pane: &mut Self, name: &str, _, cx| pane.remove_tag_by_name(name, cx)),
            ),
            on_edit: Rc::new(
                cx.listener(move |pane: &mut Self, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    pane.edit_tags(index, cx);
                    pane.tag_field.take_focus(window);
                }),
            ),
        };
        let input = rows::RowInput {
            display: &display,
            preview,
            pending,
            readme_note: if open {
                self.readme_failed_rows.get(&repo_rowid).cloned()
            } else {
                None
            },
            chevron,
            focused: self.focused_row == index,
            rowid: repo_rowid,
            actions: &actions,
        };
        rows::render_row(&input)
    }

    /// The empty state: the message says what is empty. A filter that
    /// matches nothing is not an empty library — and the reader must be
    /// able to clear the filter.
    fn render_empty_body(&mut self) -> gpui::Div {
        let filtered = self.sidebar.filter().tag_ids.is_empty();
        let message = if filtered {
            "the library is empty — run `yagna sync` first"
        } else {
            "no repository carries these tags — clear the filter"
        };
        div().flex_1().min_w_0().flex().flex_col().child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(RowLayout::top_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(message.to_string())),
        )
    }

    /// The tab bar: the permanent surfaces, then one chip per open
    /// repository tab.
    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let focus = cx.listener(|pane: &mut Self, index: &usize, _, cx| pane.focus_tab(*index, cx));
        let close =
            cx.listener(|pane: &mut Self, index: &usize, _, cx| pane.close_tab_at(*index, cx));
        let surface = cx.listener(|pane: &mut Self, which: &tabs::Surface, _, cx| {
            pane.focus_surface(*which, cx)
        });
        let actions = tabs::TabActions {
            on_focus: Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                focus(&index, window, cx)
            }),
            on_close: Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                close(&index, window, cx)
            }),
            on_surface: Rc::new(
                move |which: tabs::Surface, window: &mut Window, cx: &mut App| {
                    surface(&which, window, cx)
                },
            ),
            tip: self.tooltip.clone(),
        };
        tabs::render_bar(&self.tabs, &actions)
    }

    /// The focused tab's detail, over the list. The list stays rendered
    /// underneath — closing the last tab brings it back as it was. The
    /// user tab renders over the list the same way.
    fn render_tab_overlay(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let index = self.tabs.focused_index()?;
        let tab = self.tabs.tab(index)?;
        let content = if tab.is_local() {
            let actions = self.local_tab_actions(cx);
            localtab::render(tab, &actions)
        } else {
            let actions = self.tab_actions(cx);
            repo::render_tab(tab, &actions)
        };
        Some(
            div()
                .id("tab-overlay")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(theme::background())
                .child(content),
        )
    }

    /// The note's editor, when a note is being written: the popup over
    /// the page, with the paragraph in it and the two buttons.
    fn render_note_popup(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let subject = self.note_editing?;
        let has_note = !self.note_text(subject).is_empty();
        let actions = self.note_actions(cx);
        let field = &self.note_field;
        Some(notedialog::render(field, has_note, &actions, window).into_any_element())
    }

    /// The closures the note's popup calls.
    fn note_actions(&self, cx: &mut Context<Self>) -> NoteActions {
        let key = cx.listener(|pane: &mut Self, event: &KeyDownEvent, window, cx| {
            pane.on_note_key(event, window, cx)
        });
        let seek = cx.listener(|pane: &mut Self, index: &usize, window, cx| {
            pane.on_note_seek(*index, window, cx)
        });
        let save = cx.listener(|pane: &mut Self, _, _, cx| pane.save_note(cx));
        let delete = cx.listener(|pane: &mut Self, _, _, cx| pane.delete_note(cx));
        let close = cx.listener(|pane: &mut Self, _, _, cx| pane.close_note(cx));
        NoteActions {
            on_key: Rc::new(move |event, window, cx| key(event, window, cx)),
            on_seek: Rc::new(move |index, window, cx| seek(&index, window, cx)),
            on_save: Rc::new(move |window, cx| save(&ClickEvent::default(), window, cx)),
            on_delete: Rc::new(move |window, cx| delete(&ClickEvent::default(), window, cx)),
            on_close: Rc::new(move |window, cx| close(&ClickEvent::default(), window, cx)),
        }
    }

    /// A click in the note's paragraph: the caret seeks the click.
    fn on_note_seek(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.note_field.move_caret_to(index);
        let focus = self.note_field.focus().clone();
        window.focus(&focus);
        cx.notify();
    }

    /// The reader's own numbers: the catalog's aggregates, fetched
    /// fresh on every open.
    fn render_user_overlay(&mut self) -> gpui::Div {
        let content = match (&self.user_data, self.user_loading) {
            (Some(data), _) => usertab::render(data).into_any_element(),
            (None, true) => centered_note("counting…").into_any_element(),
            (None, false) => centered_note("the numbers could not be counted — the store said so")
                .into_any_element(),
        };
        div()
            .size_full()
            .bg(theme::background())
            .child(div().id("user-surface").size_full().child(content))
    }

    /// The DISCOVER tab: the one query shape, its controls, and the
    /// catalog-style rows over shadow rows — every row clickable.
    fn render_discover_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let controls = self.render_discover_controls(window, cx);
        let budget = discover::render_budget(&self.discover);
        let open = cx.listener(|pane: &mut Self, snapshot: &RepoSnapshot, _, cx| {
            pane.open_shadow(snapshot.clone(), cx)
        });
        let open_handler: discover::OpenHandler = Rc::new(
            move |snapshot: RepoSnapshot, window: &mut Window, cx: &mut App| {
                open(&snapshot, window, cx)
            },
        );
        let mut column = div()
            .id("discover-surface")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4();
        column = column.child(surface_title("Discover"));
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(controls)
                .child(self.render_discover_button(cx))
                .when_some(budget, |row, line| row.child(line)),
        );
        column = column.child(discover::render_hint());
        if let Some(note) = self.discover.note.clone() {
            column = column.child(counter_note(&note));
        }
        let results: Vec<RepoSnapshot> = self.discover.results.clone();
        let first: Option<RepoSnapshot> = results.first().cloned();
        column = column.child(
            div()
                .id("discover-results")
                .max_h(DISCOVER_RESULTS_HEIGHT)
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .child(discover::render_results(&results, &open_handler).into_any_element()),
        );
        if first.is_some() {
            column = column.child(control::button(
                "discover-open-first",
                "open first",
                cx.listener(move |pane, _, _, cx| {
                    if let Some(found) = first.clone() {
                        pane.open_shadow(found, cx);
                    }
                }),
            ));
        }
        column = column.child(self.render_discover_frame(cx));
        div().size_full().bg(theme::background()).child(column)
    }

    /// The five labelled controls and the sort picker, with their own
    /// key handlers.
    fn render_discover_controls(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let days =
            cx.listener(|pane, event, window, cx| pane.on_discover_days_key(event, window, cx));
        let stars =
            cx.listener(|pane, event, window, cx| pane.on_discover_stars_key(event, window, cx));
        let language =
            cx.listener(|pane, event, window, cx| pane.on_discover_language_key(event, window, cx));
        let topic =
            cx.listener(|pane, event, window, cx| pane.on_discover_topic_key(event, window, cx));
        let word =
            cx.listener(|pane, event, window, cx| pane.on_discover_word_key(event, window, cx));
        let keys = discover::ControlKeys {
            days: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    days(event, window, cx)
                },
            ),
            stars: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    stars(event, window, cx)
                },
            ),
            language: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    language(event, window, cx)
                },
            ),
            topic: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    topic(event, window, cx)
                },
            ),
            word: Rc::new(
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    word(event, window, cx)
                },
            ),
        };
        let mut controls = discover::render_controls(
            &self.discover,
            &discover::DiscoverFields {
                days: &self.discover_days_field,
                stars: &self.discover_stars_field,
                language: &self.discover_language_field,
                topic: &self.discover_topic_field,
                word: &self.discover_word_field,
            },
            window,
            &keys,
        );
        let dropdowns = self.dropdown_actions(cx);
        let sort = discover::render_sort(
            &self.discover,
            self.dropdown == Some(dropdown::Site::DiscoverSort),
            self.dropdown_highlight,
            &self.discover_sort_focus,
            &dropdowns,
        );
        controls = controls.child(sort);
        controls
    }

    /// The frame under the results: the loud one when the last run
    /// failed, the quiet one when it matched nothing.
    fn render_discover_frame(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        if let Some(problem) = self.discover.error.clone() {
            return discover::render_error_frame(&problem);
        }
        if self.discover.matched_nothing() {
            let clear = cx.listener(|pane, _, _, cx| pane.clear_discover_filters(cx));
            let label = self.discover.query_label();
            return discover::render_empty_frame(&label, clear);
        }
        if !self.discover.searched && !self.discover.running {
            return discover::render_idle_frame();
        }
        div()
    }

    /// The DISCOVER search button.
    fn render_discover_button(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let label = if self.discover.running {
            "searching…"
        } else {
            "search"
        };
        control::primary_button(
            "discover-run",
            label,
            cx.listener(|pane, _, _, cx| pane.run_discover(cx)),
        )
    }

    /// The PEOPLE tab: the followed logins, each a drawer of their own,
    /// and the suggestions above which stay a way to find new people.
    /// The surface scrolls in its own right — a long list of drawers
    /// never runs past the window's bottom.
    fn render_people_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let follow = self.follow_field.render(
            window,
            cx.listener(|pane, event, window, cx| pane.on_follow_key(event, window, cx)),
            {
                let focus = self.follow_field.focus().clone();
                move |window, _| window.focus(&focus)
            },
        );
        let actions = self.person_actions(cx);
        let mut column = div()
            .id("people-surface")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .overflow_y_scroll()
            .scrollbar_width(repo::TabLayout::SCROLLBAR);
        let mut logins = self.settings.followed_users.clone();
        logins.sort_by_key(|login| login.to_lowercase());
        column = column.child(surface_title("People"));
        column = column.child(surface_heading(&people::following_heading(logins.len())));
        if let Some(note) = self.people.note.clone() {
            column = column.child(counter_note(&note));
        }
        column = logins.iter().fold(column, |column, login| {
            let shadows = self.people.shadows.get(login).cloned().unwrap_or_default();
            let stamp = self.people.stamps.get(login).cloned();
            let open = self.people.open_drawer.as_deref() == Some(login.as_str());
            column.child(people::render_person(
                login,
                &shadows,
                stamp.as_deref(),
                open,
                &actions,
            ))
        });
        column = column.child(surface_heading(people::FIND_MORE));
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(follow.w(px(220.0)))
                .child(counter_note(
                    "enter follows · refresh is per user, never automatic",
                )),
        );
        column = column.child(self.render_suggestions(cx));
        // The wrapper is a flex column of its own: the surface's
        // `flex_1` needs one to bound its height, and without the
        // bound the scroll never engages — the list simply ran past
        // the window's bottom.
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .child(column)
    }

    /// One person's drawer actions: the toggle, the per-user refresh,
    /// the unfollow, and the repository click that opens a shadow tab.
    fn person_actions(&mut self, cx: &mut Context<Self>) -> people::PersonActions {
        let on_toggle = cx.listener(|pane: &mut Self, login: &str, _, cx| {
            pane.people.toggle_drawer(login);
            cx.notify();
        });
        let on_refresh = cx.listener(|pane: &mut Self, login: &str, _, cx| {
            let login = login.to_string();
            pane.refresh_person(&login, cx);
        });
        let on_unfollow = cx.listener(|pane: &mut Self, login: &str, _, cx| {
            let login = login.to_string();
            pane.unfollow_login(&login, cx);
        });
        let on_open = cx.listener(|pane: &mut Self, snapshot: &RepoSnapshot, _, cx| {
            pane.open_shadow(snapshot.clone(), cx)
        });
        people::PersonActions {
            on_toggle: Rc::new(move |login: &str, window: &mut Window, cx: &mut App| {
                on_toggle(login, window, cx)
            }),
            on_refresh: Rc::new(move |login: &str, window: &mut Window, cx: &mut App| {
                on_refresh(login, window, cx)
            }),
            on_unfollow: Rc::new(move |login: &str, window: &mut Window, cx: &mut App| {
                on_unfollow(login, window, cx)
            }),
            on_open_repo: Rc::new(
                move |snapshot: RepoSnapshot, window: &mut Window, cx: &mut App| {
                    on_open(&snapshot, window, cx)
                },
            ),
        }
    }

    /// The suggestions block: the logins the reader could follow. A
    /// click follows one, and it stops being a suggestion.
    fn render_suggestions(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let follow = cx.listener(|pane: &mut Self, login: &str, _, cx| {
            let login = login.to_string();
            pane.follow(login, cx);
        });
        let handler: people::FollowHandler =
            Rc::new(move |login: &str, window: &mut Window, cx: &mut App| {
                follow(login, window, cx)
            });
        people::render_suggestions(&self.people, &handler)
    }

    /// The DISCOVER days field: digits in, enter runs.
    fn on_discover_days_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.discover_days_field.on_key(event) {
            FieldAction::Submitted(_) => self.run_discover(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The DISCOVER stars field: digits in, enter runs.
    fn on_discover_stars_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.discover_stars_field.on_key(event) {
            FieldAction::Submitted(_) => self.run_discover(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The DISCOVER language field: text in, enter runs.
    fn on_discover_language_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.discover_language_field.on_key(event) {
            FieldAction::Submitted(_) => self.run_discover(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The DISCOVER topic field: text in, enter runs.
    fn on_discover_topic_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.discover_topic_field.on_key(event) {
            FieldAction::Submitted(_) => self.run_discover(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The DISCOVER word field: text in, enter runs.
    fn on_discover_word_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.discover_word_field.on_key(event) {
            FieldAction::Submitted(_) => self.run_discover(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The follow field: enter follows the typed login.
    fn on_follow_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.follow_field.on_key(event) {
            FieldAction::Submitted(_) => self.follow_login(cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The main column: the tab bar (always present), and the body —
    /// the list, the permanent surface in front, or the focused
    /// repository's detail.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let mut column = div().flex_1().min_w_0().flex().flex_col();
        column = column.child(self.render_tab_bar(cx));
        let mut children: Vec<gpui::AnyElement> = Vec::new();
        match (
            self.tabs.repo_in_front(),
            self.render_tab_overlay(window, cx),
        ) {
            (true, Some(overlay)) => children.push(overlay.into_any_element()),
            _ => children.push(self.render_surface(window, cx).into_any_element()),
        }
        if let Some(notification) = self.render_notification(cx) {
            children.push(notification.into_any_element());
        }
        column.child(body_shell(children))
    }

    /// The permanent surface in front: the library's rows, the
    /// discovery surface, the people surface, or the reader's own
    /// numbers.
    fn render_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        match self.tabs.surface() {
            tabs::Surface::Library => self.render_library(window, cx),
            tabs::Surface::Discover => self.render_discover_overlay(window, cx),
            tabs::Surface::People => self.render_people_overlay(window, cx),
            tabs::Surface::User => self.render_user_overlay(),
            tabs::Surface::Local => self.render_local_overlay(cx),
            tabs::Surface::Score => self.render_score_overlay(window, cx),
        }
    }

    /// The notification slot: the cards waiting in the shell's corner,
    /// over the body. Each one is dismissed by its own button.
    fn render_notification(&mut self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.notifications.is_empty() {
            return None;
        }
        let dismiss =
            cx.listener(|pane: &mut Self, index: &usize, _, _| pane.dismiss_notification(*index));
        let handler: shell::DismissHandler =
            Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                dismiss(&index, window, cx)
            });
        let cards: Vec<gpui::AnyElement> = self
            .notifications
            .iter()
            .enumerate()
            .map(|(index, notification)| {
                shell::render_notification(notification, index, &self.tooltip, &handler)
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .absolute()
                .bottom_3()
                .right_3()
                .flex()
                .flex_col()
                .gap_2()
                .children(cards),
        )
    }

    /// Removes the card the reader dismissed.
    fn dismiss_notification(&mut self, index: usize) {
        if index < self.notifications.len() {
            self.notifications.remove(index);
        }
    }

    /// The library: the header, the search, the rows and the footer.
    fn render_library(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        if self.total == 0 && !self.loading && self.error.is_none() && !self.is_searching() {
            return self.render_empty_body();
        }
        let header = self.render_header(cx);
        let search = self.render_search(window, cx);
        let deep = self.deep_open.then(|| self.render_deep_panel(window, cx));
        let list = self.render_list(cx);
        let footer = self.render_footer(window, cx);
        let stack = list_stack(header, search, list, footer);
        match deep {
            Some(panel) => stack.child(self.anchor_deep_panel(panel, window)),
            None => stack,
        }
    }

    /// Where the deep-search panel sits: under the list on a wide
    /// window, and as a sheet over it on a narrow one. The panel is
    /// one component — only its anchor changes.
    fn anchor_deep_panel(&self, panel: gpui::Div, window: &mut Window) -> gpui::Div {
        if !narrow_window(window) {
            return panel;
        }
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .flex_col()
            .justify_end()
            .child(panel)
    }

    /// The footer: prev/next, the page indicator, the jump-to-page
    /// field, and the loading/error status — on its own ground with a
    /// rule above it, visibly a bar and never more list.
    fn render_footer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let prev = self.page > 1;
        let next = self.page < self.page_count();
        let indicator = SharedString::from(format!("page {} of {}", self.page, self.page_count()));
        let jump = self
            .jump_field
            .render(
                window,
                cx.listener(|pane, event, window, cx| pane.on_jump_key(event, window, cx)),
                {
                    let focus = self.jump_field.focus().clone();
                    move |window, _| window.focus(&focus)
                },
            )
            .w(field::JUMP_WIDTH);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(theme::surface_alt())
            .border_t_1()
            .border_color(theme::border())
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .child(page_button(
                "‹ prev",
                prev,
                cx.listener(|pane, _, _, cx| {
                    pane.go_to_page(pane.page.saturating_sub(1), cx);
                }),
            ))
            .child(indicator)
            .child(page_button(
                "next ›",
                next,
                cx.listener(|pane, _, _, cx| {
                    pane.go_to_page(pane.page.saturating_add(1), cx);
                }),
            ))
            .child(jump)
            .child(div().flex_1())
            .child(self.render_status())
    }

    /// The jump-to-page field's key handling: digits into the buffer,
    /// enter jumps, escape clears.
    fn on_jump_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match self.jump_field.on_key(event) {
            FieldAction::Submitted(text) => self.apply_jump(&text, cx),
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// Jumps to the typed page number, when it names a real page.
    fn apply_jump(&mut self, typed: &str, cx: &mut Context<Self>) {
        self.jump_field.clear();
        match typed.trim().parse::<u32>() {
            Ok(page) => self.go_to_page(page, cx),
            Err(_) => cx.notify(),
        }
    }

    /// The footer's status slot: what the pane is waiting on, or what
    /// went wrong.
    fn render_status(&self) -> gpui::Div {
        if let Some(error) = &self.error {
            return div()
                .text_size(RowLayout::small_size())
                .text_color(theme::health_low())
                .child(SharedString::from(error.clone()));
        }
        if let Some(notice) = &self.notice {
            return div()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(notice.clone()));
        }
        div()
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .when(self.loading, |status| status.child("loading…"))
    }

    /// The sidebar: a column of drawers — tags, topics, notes — over a
    /// filter box that narrows all three, scrolling in its own right
    /// so a reader with hundreds of tags can reach the bottom. The tag
    /// editor replaces the column when a row asked for one.
    fn render_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut column = div()
            .id("sidebar-column")
            .w(SIDEBAR_WIDTH)
            .flex()
            .flex_col()
            .min_h_0()
            .bg(theme::surface())
            .border_r_1()
            .border_color(theme::border())
            .overflow_y_scroll()
            .scrollbar_width(repo::TabLayout::SCROLLBAR);
        if let Some(repo_rowid) = self.sidebar.edited() {
            column = column.child(self.render_tag_editor(window, repo_rowid, cx));
        } else {
            column = column.child(self.render_tag_filter(window, cx));
        }
        column
    }

    /// The filter column: the filter row with its clear-all, the
    /// filter box, and the three drawers.
    fn render_tag_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let mode_label = match self.sidebar.match_mode() {
            yagna_store::TagMatch::All => "all",
            yagna_store::TagMatch::Any => "any",
        };
        let active = self.sidebar.active_summary();
        let mut column = div().flex().flex_col().gap_1().p_2();
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(tabs::icon_element(icons::Icon::Filter, theme::text_muted()))
                        .child(SharedString::from(
                            active.unwrap_or_else(|| "filter".to_string()),
                        )),
                )
                .child(
                    div()
                        .id("tag-mode")
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_alt()))
                        .text_color(theme::accent())
                        .on_click(cx.listener(|pane, _, _, cx| {
                            pane.apply_sidebar_action(SidebarAction::ToggleMode, cx)
                        }))
                        .child(SharedString::from(format!("match {mode_label}"))),
                ),
        );
        if self.sidebar.has_selection() {
            column = column.child(control::button(
                "tag-clear",
                "clear all filters",
                cx.listener(|pane, _, _, cx| {
                    pane.apply_sidebar_action(SidebarAction::ClearFilter, cx)
                }),
            ));
        }
        column = column.child(self.render_sidebar_filter_box(window, cx));
        for kind in DrawerKind::ALL {
            column = column.child(self.render_drawer(kind, window, cx));
        }
        if !self.sidebar.notice().is_empty() {
            column = column.child(
                div()
                    .mt_2()
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .child(SharedString::from(self.sidebar.notice().to_string())),
            );
        }
        column
    }

    /// The sidebar's filter box: one word, narrowing the tags, the
    /// topics and the notes at once.
    fn render_sidebar_filter_box(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let field = self.sidebar_filter_field.render(
            window,
            cx.listener(|pane, event, window, cx| pane.on_sidebar_filter_key(event, window, cx)),
            {
                let focus = self.sidebar_filter_field.focus().clone();
                move |window, _| window.focus(&focus)
            },
        );
        div().flex().flex_col().gap_1().child(field)
    }

    /// The filter box's key handling: every change narrows the lists.
    fn on_sidebar_filter_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.sidebar_filter_field.on_key(event) {
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => {
                self.sidebar.set_filter(self.sidebar_filter_field.value());
                cx.notify();
            }
            FieldAction::Submitted(_) => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// One drawer: the header line (the name, the count, the chevron)
    /// and, when open, the entries. Only one drawer is open at a time;
    /// a drawer with nothing in it says "nothing here" and shows no
    /// zero.
    fn render_drawer(
        &mut self,
        kind: DrawerKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = self.sidebar.open_drawer() == Some(kind);
        let count = self.drawer_count(kind);
        let mut column = div().flex().flex_col().gap_1().mt_2();
        column = column.child(self.render_drawer_header(kind, count, open, cx));
        if !open {
            return column;
        }
        column = column.child(self.render_drawer_body(kind, count, window, cx));
        column
    }

    /// One drawer's header: the name, the count of entries the reader
    /// can act on, and the chevron — the shell's chrome, no colour of
    /// its own.
    fn render_drawer_header(
        &mut self,
        kind: DrawerKind,
        count: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let icon = match kind {
            DrawerKind::Tags => icons::Icon::Tag,
            DrawerKind::Topics => icons::Icon::Book,
            DrawerKind::Notes => icons::Icon::User,
            DrawerKind::Activity => icons::Icon::Refresh,
        };
        let count_label = if count > 0 {
            count.to_string()
        } else {
            String::new()
        };
        div()
            .id(SharedString::from(format!("drawer-{}", kind.name())))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(if open {
                theme::tab_active()
            } else {
                theme::surface()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.apply_sidebar_action(SidebarAction::ToggleDrawer(kind), cx)
            }))
            .child(tabs::icon_element(icon, theme::text_muted()))
            .child(SharedString::from(kind.name().to_string()))
            .child(div().flex_1())
            .when(!count_label.is_empty(), |header| {
                header.child(SharedString::from(count_label))
            })
            .child(SharedString::from(if open { "▾" } else { "▸" }))
    }

    /// How many entries a drawer holds after the filter box's word.
    fn drawer_count(&self, kind: DrawerKind) -> usize {
        let word = self.sidebar.filter_text();
        match kind {
            DrawerKind::Tags => self
                .sidebar
                .tags()
                .iter()
                .filter(|tag| tag.name.to_lowercase().contains(word))
                .count(),
            DrawerKind::Topics => self
                .sidebar
                .topics()
                .iter()
                .filter(|topic| topic.name.to_lowercase().contains(word))
                .count(),
            DrawerKind::Notes => self
                .sidebar
                .notes()
                .iter()
                .filter(|note| {
                    note.title().to_lowercase().contains(word)
                        || note.note.to_lowercase().contains(word)
                })
                .count(),
            DrawerKind::Activity => self
                .sidebar
                .activity()
                .iter()
                .filter(|row| row.to_lowercase().contains(word))
                .count(),
        }
    }

    /// One drawer's open body: the entries, in the drawer's own shape.
    /// The count arrives from the header's pass — one filter walk per
    /// frame, not three.
    fn render_drawer_body(
        &mut self,
        kind: DrawerKind,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut column = div().flex().flex_col().gap_1();
        match kind {
            DrawerKind::Tags => {
                column = self.render_tag_entries(window, column, cx);
            }
            DrawerKind::Topics => {
                column = self.render_topic_entries(column, cx);
            }
            DrawerKind::Notes => {
                column = self.render_note_entries(column, cx);
            }
            DrawerKind::Activity => {
                column = self.render_activity_entries(column, cx);
            }
        }
        if count == 0 {
            column = column.child(sidebar_quiet_line("nothing here"));
        }
        if kind == DrawerKind::Topics {
            // The topic list is a fact about the stars, never a thing
            // the reader maintains — said once, under the list.
            column = column.child(sidebar_quiet_line(
                "topics come from your stars — unstar a repository to remove its topic",
            ));
        }
        column
    }

    /// The tag drawer's entries, filtered by the filter box's word.
    fn render_tag_entries(
        &mut self,
        window: &mut Window,
        column: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let word = self.sidebar.filter_text().to_string();
        let tags: Vec<yagna_store::TagRecord> = self
            .sidebar
            .tags()
            .iter()
            .filter(|tag| tag.name.to_lowercase().contains(&word))
            .cloned()
            .collect();
        tags.into_iter().fold(column, |column, tag| {
            column.child(self.render_tag_row(window, tag, cx))
        })
    }

    /// The topic drawer's entries: the catalog's topics with their
    /// counts, one filter at a time, a topic with nothing in the
    /// current filter dimmed but never hidden. The heaviest
    /// `TOPICS_DRAWN` are painted and the rest named — a catalog this
    /// size carries thousands of topics, and painting every one each
    /// frame is the lag the cap answers.
    fn render_topic_entries(&mut self, column: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let word = self.sidebar.filter_text().to_string();
        let selected = self.sidebar.topic().map(str::to_string);
        let matched: Vec<yagna_store::NameCount> = self
            .sidebar
            .topics()
            .iter()
            .filter(|topic| topic.name.to_lowercase().contains(&word))
            .cloned()
            .collect();
        let hidden = hidden_topic_count(matched.len());
        let drawn = matched
            .into_iter()
            .take(TOPICS_DRAWN)
            .fold(column, |column, topic| {
                column.child(self.render_topic_row(&topic, selected.as_deref(), cx))
            });
        if hidden == 0 {
            return drawn;
        }
        drawn.child(sidebar_quiet_line(&format!(
            "…and {hidden} more — narrow the filter to see them"
        )))
    }

    /// One topic row: the name, the count, and the dimming that says
    /// the filter — not the topic — is what emptied it.
    fn render_topic_row(
        &mut self,
        topic: &yagna_store::NameCount,
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let name = topic.name.clone();
        let is_selected = selected == Some(topic.name.as_str());
        div()
            .id(SharedString::from(format!("topic-{name}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .text_color(if is_selected {
                theme::accent()
            } else if topic.count == 0 {
                theme::text_muted()
            } else {
                theme::text()
            })
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.apply_sidebar_action(SidebarAction::ToggleTopic(name.clone()), cx)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(SharedString::from(topic.name.clone())),
            )
            .child(
                div()
                    .text_color(theme::text_muted())
                    .child(SharedString::from(topic.count.to_string())),
            )
    }

    /// The notes drawer's entries: the repository's name with the
    /// note's first words beside it — a click opens the repository.
    fn render_note_entries(&mut self, column: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let word = self.sidebar.filter_text().to_string();
        let notes: Vec<yagna_store::NoteRecord> = self
            .sidebar
            .notes()
            .iter()
            .filter(|note| {
                let haystack = format!("{} {}", note.title(), note.note).to_lowercase();
                haystack.contains(&word)
            })
            .cloned()
            .collect();
        notes.into_iter().fold(column, |column, note| {
            column.child(self.render_note_row(&note, cx))
        })
    }

    /// One note row, capped at three lines: the repository's name on
    /// the first, the note's own words on the next two — and a click
    /// anywhere on the row opens the repository.
    fn render_note_row(
        &mut self,
        note: &yagna_store::NoteRecord,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let subject = note.subject;
        let title = note.title();
        let body = note.note.trim().to_string();
        div()
            .id(SharedString::from(format!("note-{subject:?}")))
            .flex()
            .flex_col()
            .gap_0()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.apply_sidebar_action(SidebarAction::OpenNote(subject), cx)
            }))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_color(theme::text())
                    .child(SharedString::from(title)),
            )
            .when(!body.is_empty(), |row| {
                row.child(
                    div()
                        .line_clamp(2)
                        .overflow_hidden()
                        .text_color(theme::text_muted())
                        .child(SharedString::from(body)),
                )
            })
    }

    /// One tag of the drawer's list: its name (which selects it), its
    /// count, and the three management buttons — rename, merge, delete.
    /// A delete asks for its confirmation in the row — a confirm and a
    /// cancel pair where the delete button was — and a click anywhere
    /// else cancels it.
    fn render_tag_row(
        &mut self,
        window: &mut Window,
        tag: yagna_store::TagRecord,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self.sidebar.is_selected(tag.id);
        let armed_delete = self.sidebar.armed_delete() == Some(tag.id);
        let armed_merge = self.sidebar.armed_merge();
        let renaming = self.sidebar.renaming() == Some(tag.id);
        let label: gpui::AnyElement = if renaming {
            self.rename_field
                .render(
                    window,
                    cx.listener(|pane, event, window, cx| pane.on_rename_key(event, window, cx)),
                    {
                        let focus = self.rename_field.focus().clone();
                        move |window, _| window.focus(&focus)
                    },
                )
                .w(px(110.0))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_color(if selected {
                    theme::accent()
                } else {
                    theme::text()
                })
                .child(SharedString::from(tag.name.clone()))
                .into_any_element()
        };
        let count = div()
            .text_size(RowLayout::small_size())
            .text_color(theme::text_muted())
            .child(SharedString::from(tag.repos.to_string()));
        if armed_delete {
            // The delete's confirmation, in the row: confirm runs it,
            // cancel stands down, and a click anywhere else cancels it
            // through the pane's click-outside rule.
            let confirm = self.render_tag_button(
                "delete",
                format!("delete-{}", tag.id),
                theme::health_low(),
                SidebarAction::ConfirmDelete(tag.id),
                cx,
            );
            let cancel = self.render_tag_button(
                "cancel",
                format!("cancel-delete-{}", tag.id),
                theme::text_muted(),
                SidebarAction::CancelDelete,
                cx,
            );
            return div()
                .id(SharedString::from(format!("tag-{}", tag.id)))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_sm()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .child(label)
                .child(count)
                .child(confirm)
                .child(cancel);
        }
        let rename = self.render_tag_button(
            "✎",
            format!("rename-{}", tag.id),
            theme::text_muted(),
            SidebarAction::ArmRename(tag.id),
            cx,
        );
        let merge_label = match armed_merge {
            Some(source) if source == tag.id => "…",
            Some(_) => "into",
            None => "⇄",
        };
        let merge = self.render_tag_button(
            merge_label,
            format!("merge-{}", tag.id),
            theme::text_muted(),
            match armed_merge {
                Some(source) if source == tag.id => SidebarAction::ArmMerge(tag.id),
                Some(source) => SidebarAction::MergeInto {
                    source,
                    target: tag.id,
                },
                None => SidebarAction::ArmMerge(tag.id),
            },
            cx,
        );
        let delete = self.render_tag_button(
            "×",
            format!("delete-{}", tag.id),
            theme::text_muted(),
            SidebarAction::ArmDelete(tag.id),
            cx,
        );
        div()
            .id(SharedString::from(format!("tag-{}", tag.id)))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .child(
                div()
                    .id(SharedString::from(format!("tag-name-{}", tag.id)))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.apply_sidebar_action(SidebarAction::ToggleTag(tag.id), cx)
                    }))
                    .child(label)
                    .child(count),
            )
            .child(rename)
            .child(merge)
            .child(delete)
    }

    /// One small management button of a tag row. The button stops its
    /// own click, so arming an edit never trips the click-outside rule.
    fn render_tag_button(
        &mut self,
        label: &str,
        id: String,
        ink: gpui::Rgba,
        action: SidebarAction,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(id))
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .text_size(RowLayout::small_size())
            .text_color(ink)
            .on_click(cx.listener(move |pane, _, _, cx| {
                cx.stop_propagation();
                pane.apply_sidebar_action(action.clone(), cx)
            }))
            .child(SharedString::from(label.to_string()))
            .into_any_element()
    }

    /// The rename field's key handling: enter commits the new name,
    /// escape cancels the rename.
    fn on_rename_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.rename_field.on_key(event) {
            FieldAction::Submitted(name) => {
                let Some(tag_id) = self.sidebar.renaming() else {
                    return;
                };
                if name.trim().is_empty() {
                    self.sidebar.clear_arms("the rename was cancelled");
                    cx.notify();
                    return;
                }
                self.apply_sidebar_action(
                    SidebarAction::Rename {
                        tag_id,
                        name: name.trim().to_string(),
                    },
                    cx,
                );
            }
            FieldAction::Escaped => {
                self.sidebar.clear_arms("the rename was cancelled");
                cx.notify();
            }
            FieldAction::Changed | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }

    /// The tag editor: the edited row's tags, the tags it could carry,
    /// and the new-tag field. The editor column stops its own clicks,
    /// so working inside it never trips the click-outside rule.
    fn render_tag_editor(
        &mut self,
        window: &mut Window,
        repo_rowid: i64,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let name = self
            .rows
            .iter()
            .find(|row| row.repo_rowid == repo_rowid)
            .map(|row| row.repo_ref.name.clone())
            .unwrap_or_default();
        let mut column = div()
            .id("tag-editor-column")
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .on_click(|_, _, cx| cx.stop_propagation());
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child(SharedString::from(format!("tags of {name}")))
                .child(
                    div()
                        .id("tag-editor-close")
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_alt()))
                        .text_color(theme::accent())
                        .on_click(cx.listener(|pane, _, _, cx| {
                            pane.apply_sidebar_action(SidebarAction::CloseEditor, cx)
                        }))
                        .child("done"),
                ),
        );
        column = column.child(self.render_editor_tags(repo_rowid, cx));
        let field = self.tag_field.render(
            window,
            cx.listener(|pane, event, window, cx| pane.on_tag_key(event, window, cx)),
            {
                let focus = self.tag_field.focus().clone();
                move |window, _| window.focus(&focus)
            },
        );
        column = column.child(field);
        column
    }

    /// The edited row's tags, and the ones it could carry, from the
    /// cache the background fetch filled — the render path never opens
    /// the database (a per-frame store read would freeze the window
    /// whenever a background task held the lock).
    fn render_editor_tags(&mut self, _repo_rowid: i64, cx: &mut Context<Self>) -> gpui::Div {
        let Some(answer) = self.sidebar.editor_tags() else {
            return div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .text_size(RowLayout::small_size())
                .text_color(theme::text_muted())
                .child("the tags are on their way…");
        };
        let carried = answer.carried.clone();
        let all = answer.available.clone();
        let mut column = div().flex().flex_col().gap_1();
        for (tag_id, name) in carried {
            let label = SharedString::from(name);
            column = column.child(
                div()
                    .id(SharedString::from(format!("editor-tag-{tag_id}")))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(theme::surface_alt())
                    .text_size(RowLayout::small_size())
                    .text_color(theme::accent())
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.apply_sidebar_action(SidebarAction::RemoveTag(tag_id), cx)
                    }))
                    .child(label),
            );
        }
        for (tag_id, name) in all {
            let label = SharedString::from(format!("+ {name}"));
            column = column.child(
                div()
                    .id(SharedString::from(format!("editor-add-{tag_id}")))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_alt()))
                    .text_size(RowLayout::small_size())
                    .text_color(theme::text_muted())
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        // An existing tag keeps the kind it has: the
                        // reader is attaching it, not re-creating it.
                        pane.apply_sidebar_action(SidebarAction::AttachTag(name.clone()), cx)
                    }))
                    .child(label),
            );
        }
        column
    }

    /// The tag field's key handling: enter creates the tag.
    fn on_tag_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match self.tag_field.on_key(event) {
            FieldAction::Submitted(name) => {
                self.apply_sidebar_action(SidebarAction::AddTag { name }, cx)
            }
            FieldAction::Changed | FieldAction::Escaped | FieldAction::Moved => cx.notify(),
            FieldAction::None => {}
        }
    }
}

/// The note's first words, as the drawer shows them beside the name.
/// How many topic rows the drawer paints before it names the rest:
/// a starred catalog carries thousands of topics, and every painted
/// row is a stateful element built each frame.
const TOPICS_DRAWN: usize = 100;

/// How many of the matched topics the drawer does not paint — the
/// "…and N more" line's number.
fn hidden_topic_count(matched: usize) -> usize {
    matched.saturating_sub(TOPICS_DRAWN)
}

/// Whether the spec field follows the file on a reload: only while
/// its text is exactly what the last fill put there — a reader's
/// typed edit, any divergence, survives untouched.
fn spec_field_follows_file(field_value: &str, loaded: &str) -> bool {
    field_value == loaded
}

/// A quiet line in the sidebar's drawers: what a drawer says when it
/// holds nothing, and the one fact the topic drawer carries.
fn sidebar_quiet_line(text: &str) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

impl gpui::Render for ListPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The first frame's mark: the launch path's own clock, once.
        if !self.frame_marked {
            self.frame_marked = true;
            self.startup
                .mark_since_start(crate::startup::Slot::Frame, Instant::now());
        }
        let shell = self.render_shell(cx);
        let body = self.render_body(window, cx);
        let mut main = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .pt(chrome_height());
        if self.sidebar_open {
            main = main.child(self.render_sidebar(window, cx));
        }
        let root = div()
            .id("window-root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(Self::on_list_key))
            // The click-outside rule: any click that bubbles this far
            // was not inside a dropdown or an editor (those stop their
            // own clicks), so the transient things close.
            .on_click(cx.listener(|pane, _, _, cx| pane.cancel_if_outside(cx)))
            .child(main.child(body))
            .child(shell);
        // The library sort's open list paints above everything — the
        // header's bar is fixed-height, so the list hangs from the
        // window instead of growing inside it.
        let root = if self.dropdown == Some(dropdown::Site::LibrarySort) {
            root.child(self.render_sort_popup(cx))
        } else {
            root
        };
        // The shell's card paints above everything, menu included.
        let root = match self.dialog {
            Some(Dialog::About) => root.child(self.render_about(cx)),
            Some(Dialog::Help) => root.child(self.render_help(cx)),
            None => root,
        };
        // The note's popup paints above everything else.
        let root = match self.render_note_popup(window, cx) {
            Some(popup) => root.child(popup),
            None => root,
        };
        // The settings modal paints above everything, menu included.
        let root = if self.settings_open {
            let actions = self.settings_actions(cx);
            root.child(settings_panel::render(
                &self.settings,
                &self.cache_field,
                &self.page_field,
                &self.local_folder_field,
                &actions,
                window,
            ))
            .child(self.render_settings_footer())
        } else {
            root
        };
        root.into_any_element()
    }
}

/// What a local tab's read brings back: the entry itself, the
/// catalog's row when the origin matched one, and the note.
struct LocalTabContent {
    /// The entry, as the store holds it.
    repo: Option<LocalRepo>,
    /// The catalog's row of the same repository, when the origin
    /// matched one.
    linked: Option<yagna_core::model::RepoDetail>,
    /// The reader's note about it.
    note: Option<String>,
    /// The worktree's root, as the disk holds it.
    entries: Option<Vec<yagna_core::model::LocalEntry>>,
}

/// Whether the keystroke asks for the save: ctrl+enter. The
/// paragraph's own enter breaks the line, so the save needs its own
/// key — the popup's button is the mouse's way, this is the
/// keyboard's.
fn is_save_key(event: &KeyDownEvent) -> bool {
    event.keystroke.key.as_str() == "enter" && event.keystroke.modifiers.control
}

/// The sidebar's three lists, as one background fetch answers them:
/// the tags, the catalog's topics, and the noted repositories.
type SidebarLists = (
    Vec<yagna_store::TagRecord>,
    Vec<yagna_store::NameCount>,
    Vec<yagna_store::NoteRecord>,
);

/// A muted line: what the DISCOVER and PEOPLE panels say beside
/// their controls.
fn counter_note(text: &str) -> gpui::Div {
    div()
        .text_size(RowLayout::small_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

/// The width under which the window is narrow: the deep-search panel
/// stops sitting under the list and becomes a sheet over it.
const NARROW_WIDTH: Pixels = px(900.0);

/// Whether the window is too narrow for the panel to sit under the
/// list.
fn narrow_window(window: &mut Window) -> bool {
    window.viewport_size().width < NARROW_WIDTH
}

/// A surface's section heading: the block's name, in the heading ink.
fn surface_heading(text: &str) -> gpui::Div {
    div()
        .text_size(RowLayout::heading_size())
        .text_color(theme::text())
        .child(SharedString::from(text.to_string()))
}

/// A surface's title: the one h1 the surface wears.
fn surface_title(text: &str) -> gpui::Div {
    div()
        .text_size(typography::scaled(typography::TypeScale::SURFACE_TITLE))
        .text_color(theme::text())
        .child(SharedString::from(text.to_string()))
}

/// A centered muted note: what the user tab says while it counts.
fn centered_note(text: &str) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(RowLayout::top_size())
        .text_color(theme::text_muted())
        .child(SharedString::from(text.to_string()))
}

/// The list's stack: the header, the search row, the list, and the
/// footer — a flex column, so the list grows into the space the header,
/// the search row and the footer leave it.///
/// The column matters: a flex item's `flex_1` only distributes free
/// space inside a flex container. In a plain block the list would have
/// no height at all, and the catalog would render as an empty window.
fn list_stack(
    header: gpui::Div,
    search: gpui::Div,
    list: gpui::Div,
    footer: gpui::Div,
) -> gpui::Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(header)
        .child(search)
        .child(list)
        .child(footer)
}

/// The body's wrapper: the surface (or the focused tab's detail) and
/// the cards that float over it. The wrapper matters twice: a flex
/// item's `flex_1` only distributes free space inside a flex
/// container, so in a plain block the list would have no height at
/// all; and the wrapper is the positioned ancestor the overlays (the
/// tab's detail, the notifications) hang against.
fn body_shell(children: Vec<gpui::AnyElement>) -> gpui::Div {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .children(children)
}

/// One page of the session: the catalog's page, or the ranked matches
/// when a query is typed. The ranking is the fuzzy score; the sort key
/// breaks a tie.
fn fetch_page(
    store: &Store,
    index: &Arc<Mutex<FuzzyIndex>>,
    sort: SortKey,
    offset: u32,
    limit: u32,
    filter: &CatalogFilter,
    query: &str,
) -> Result<(Vec<RepoRow>, u64), StoreError> {
    if query.trim().is_empty() {
        let rows = store.page(sort, offset, limit, filter)?;
        let total = store.total_count(filter)?;
        return Ok((rows, total));
    }
    let candidates = store.candidate_ids(sort, filter)?;
    let order: HashMap<i64, usize> = candidates
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, position))
        .collect();
    let hits: Vec<FuzzyHit> = lock_index(index)
        .search(query)
        .into_iter()
        .filter(|hit| order.contains_key(&hit.repo_rowid))
        .collect();
    let total = hits.len() as u64;
    let page: Vec<i64> = hits
        .iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|hit| hit.repo_rowid)
        .collect();
    let rows = store.rows_by_ids(&page)?;
    Ok((rows, total))
}

/// One row's preview: the README's rendered head and the image. The
/// image is fetched once and cached on disk; a failure leaves the
/// placeholder.
///
/// The README comes from the store's *rewritten* body — the text whose
/// image destinations are the asset ids the store minted — so a
/// relative image resolves to its cached file. The row's whole asset
/// set comes with it, so the preview's answer book covers every image
/// the head mentions.
fn read_preview(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    cache_dir: &Path,
    repo_rowid: i64,
) -> Result<RowPreview, String> {
    let (head, assets, asset) = {
        let store = lock_catalog(catalog);
        let head = store
            .readme_body(repo_rowid as u64)
            .map_err(|error| error.to_string())?;
        let assets = store
            .repo_assets(repo_rowid as u64)
            .map_err(|error| error.to_string())?;
        let asset = store
            .preview_asset(repo_rowid as u64)
            .map_err(|error| error.to_string())?;
        (head, assets, asset)
    };
    let asset = fetch_preview_asset(catalog, services, cache_dir, asset)?;
    Ok(RowPreview {
        head,
        assets,
        asset,
    })
}

/// The preview image's bytes: the cached file when it is there, a fetch
/// when it is not. The fetch is anonymous and capped; a failure leaves
/// the asset as it was — the row shows a placeholder, never an error.
fn fetch_preview_asset(
    catalog: &Arc<Mutex<Store>>,
    services: &Services,
    cache_dir: &Path,
    asset: Option<yagna_store::AssetRecord>,
) -> Result<Option<yagna_store::AssetRecord>, String> {
    let Some(record) = asset else {
        return Ok(None);
    };
    if record.state == "present" {
        let shown = record
            .path
            .as_ref()
            .is_some_and(|path| assets::file_is_rasterisable(path));
        return Ok(if shown { Some(record) } else { None });
    }
    let Some(forge) = services.assets.as_ref() else {
        return Ok(Some(record));
    };
    let Some(host) = services.host.as_ref() else {
        return Ok(Some(record));
    };
    let asset_id = record.asset_id.clone();
    let url = record.url.clone();
    {
        let mut store = lock_catalog(catalog);
        store
            .enqueue_asset(&asset_id, Utc::now())
            .map_err(|error| error.to_string())?;
    }
    let bytes = host
        .block_on(forge.fetch_asset(&url))
        .map_err(|error| error.describe())?;
    if !assets::is_rasterisable(&bytes) {
        // The rasteriser would abort the process on this one; the row
        // shows its placeholder instead.
        return Ok(None);
    }
    let mut store = lock_catalog(catalog);
    let stored = store
        .store_asset_bytes(&asset_id, "", &bytes, cache_dir, Utc::now())
        .map_err(|error| error.to_string())?;
    Ok(Some(stored))
}

/// A pager button: interactive when enabled, visually inert when not.
fn page_button(
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .flex()
        .flex_row()
        .items_center()
        .h(control::CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .border_2()
        .border_color(gpui::transparent_black())
        .text_size(RowLayout::small_size())
        .text_color(if enabled {
            theme::text()
        } else {
            theme::text_muted()
        })
        .map(|button| {
            if enabled {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_alt()))
                    .on_click(on_click)
            } else {
                button
            }
        })
        .child(label)
}

/// Locks the shared catalog handle, recovering from a poisoned lock: a
/// panic in one fetch must not take the catalog down with it.
fn lock_catalog(catalog: &Arc<Mutex<Store>>) -> MutexGuard<'_, Store> {
    catalog
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Locks the fuzzy index, recovering from a poisoned lock the same way.
fn lock_index(index: &Arc<Mutex<FuzzyIndex>>) -> MutexGuard<'_, FuzzyIndex> {
    index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The row the renderer falls back to when the page and its view models
/// disagree mid-flip: an empty row paints nothing meaningful and the
/// next fetch corrects the pane.
fn stub_row() -> RepoRow {
    RepoRow {
        repo_rowid: 0,
        repo_ref: yagna_core::model::RepoRef {
            forge: yagna_core::model::Forge::GitHub,
            owner: String::new(),
            name: String::new(),
        },
        description: None,
        stars: 0,
        primary_language: None,
        license_name: None,
        topics: Vec::new(),
        created_at: chrono::DateTime::<Utc>::UNIX_EPOCH,
        starred_at: None,
        last_commit_at: chrono::DateTime::<Utc>::UNIX_EPOCH,
        first_commit_date: None,
        health: yagna_core::health::HealthReport::default(),
        fit_score: None,
        tags: Vec::new(),
        excerpt: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn key_event(key: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(key).expect("test keystroke"),
            is_held: false,
        }
    }

    /// The bug this pins: the body's wrapper was a plain block, so the
    /// list's `flex_1` had no free space to grow into — the catalog
    /// rendered as an empty window, with the footer under the search
    /// row and nothing below it.
    #[test]
    fn the_list_grows_inside_a_flex_column() {
        let mut body = body_shell(vec![
            list_stack(div(), div(), div(), div()).into_any_element(),
        ]);
        let style = body.style();
        assert_eq!(
            style.display,
            Some(gpui::Display::Flex),
            "the body is a flex container"
        );
        assert_eq!(
            style.flex_direction,
            Some(gpui::FlexDirection::Column),
            "the body stacks its children"
        );
        assert_eq!(style.flex_grow, Some(1.0), "the body fills the column");

        let mut stack = list_stack(div(), div(), div(), div());
        assert_eq!(
            stack.style().flex_direction,
            Some(gpui::FlexDirection::Column),
            "the stack stacks the header, the search row, the list and the footer"
        );
    }

    #[test]
    fn page_math_counts_and_offsets_pages() {
        assert_eq!(
            page_count_for(0, 500),
            1,
            "an empty catalog still shows page 1"
        );
        assert_eq!(page_count_for(1, 500), 1);
        assert_eq!(page_count_for(500, 500), 1);
        assert_eq!(page_count_for(501, 500), 2);
        assert_eq!(page_count_for(2_000, 500), 4);
        assert_eq!(
            page_count_for(600, 100),
            6,
            "the page size is the settings'"
        );
        assert_eq!(offset_of(1, 500), 0);
        assert_eq!(offset_of(2, 500), 500);
        assert_eq!(offset_of(4, 500), 1_500);
        assert_eq!(offset_of(3, 250), 500);
    }

    /// The settings surface's sort cycle walks every key in order and
    /// wraps.
    #[test]
    fn the_sort_cycle_walks_every_key_and_wraps() {
        let all = SortKey::ALL;
        for (index, key) in all.into_iter().enumerate() {
            let next = next_sort_key(key);
            assert_eq!(next, all[(index + 1) % all.len()]);
        }
    }

    #[test]
    fn digit_keys_are_bare_digits_only() {
        for digit in ["1", "7", "0"] {
            assert!(is_digit_key(&key_event(digit)));
        }
        assert!(!is_digit_key(&key_event("enter")));
        assert!(!is_digit_key(&key_event("backspace")));
        assert!(
            !is_digit_key(&key_event("shift-1")),
            "modified keystrokes are not page digits"
        );
        assert!(!is_digit_key(&key_event("f12")));
    }

    /// The topic drawer paints a bounded slice and names the rest:
    /// thousands of stateful rows a frame was the lag.
    #[test]
    fn the_topic_drawer_hides_nothing_below_the_cap_and_counts_the_rest() {
        assert_eq!(hidden_topic_count(0), 0);
        assert_eq!(hidden_topic_count(TOPICS_DRAWN), 0, "at the cap, all show");
        assert_eq!(hidden_topic_count(TOPICS_DRAWN - 1), 0);
        assert_eq!(hidden_topic_count(TOPICS_DRAWN + 1), 1);
        assert_eq!(hidden_topic_count(6_700), 6_700 - TOPICS_DRAWN);
    }

    /// The spec field follows the file only while the reader has not
    /// typed into it.
    #[test]
    fn the_spec_field_follows_the_file_only_while_untouched() {
        assert!(spec_field_follows_file("", ""), "a fresh field follows");
        assert!(
            spec_field_follows_file("the file's text", "the file's text"),
            "an untouched field follows"
        );
        assert!(
            !spec_field_follows_file("the reader's edit", "the file's text"),
            "a typed edit survives the reload"
        );
    }

    /// Whether a keystroke is a bare digit: one character, no modifiers.
    fn is_digit_key(event: &KeyDownEvent) -> bool {
        let key = event.keystroke.key.as_str();
        key.len() == 1
            && key.chars().next().is_some_and(|c| c.is_ascii_digit())
            && !event.keystroke.modifiers.modified()
    }
}
