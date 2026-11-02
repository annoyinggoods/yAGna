//! The sidebar's state: the filter (tags, topics, the match mode), the
//! drawers that hold them, and the editor's state.
//!
//! The sidebar is a column of drawers — tags, topics and notes — over
//! a filter box that narrows all three at once, plus the tag editor
//! for one row. A topic is a fact about the stars, aggregated from the
//! catalog; a tag is the reader's own name; a note is one repository's
//! sentence. The three share one shape in the sidebar: a drawer with a
//! count, one of them open at a time, the open one remembered across
//! launches.
//!
//! The tag management lives here too: a rename is edited in place, a
//! merge is armed on the source and named by the next tag, and a
//! delete is armed before it runs — the count of the repositories it
//! would remove is said first.
//!
//! Items: `SidebarState` — the lists, the selection, the topic, the
//! match mode, the open drawer, the edited row, the armed operations,
//! the notice; `SidebarAction` — what the sidebar asks the pane to do;
//! `DrawerKind` — the sidebar's three drawers.
//! Depends on: yagna_store (the tag records, the topic counts, the
//! note records, the filter).
//! Called by: crate::ui (the sidebar column).

use yagna_core::model::NoteSubject;
use yagna_store::{CatalogFilter, NameCount, NoteRecord, TagMatch, TagRecord};

/// The sidebar's drawers: the groups the column holds, one of them
/// open at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrawerKind {
    /// The reader's own tags.
    Tags,
    /// The catalog's GitHub topics — a fact, never edited.
    Topics,
    /// The repositories that carry a note.
    Notes,
    /// What the connected agent has been doing.
    Activity,
}

impl DrawerKind {
    /// Every drawer, in the order the column shows them.
    pub(crate) const ALL: [DrawerKind; 4] = [
        DrawerKind::Tags,
        DrawerKind::Topics,
        DrawerKind::Notes,
        DrawerKind::Activity,
    ];

    /// The drawer's name — the word the header shows and the key the
    /// persisted open state lives under.
    pub(crate) fn name(self) -> &'static str {
        match self {
            DrawerKind::Tags => "tags",
            DrawerKind::Topics => "topics",
            DrawerKind::Notes => "notes",
            DrawerKind::Activity => "activity",
        }
    }

    /// The stored name back into a drawer; unknown text is none.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "tags" => Some(DrawerKind::Tags),
            "topics" => Some(DrawerKind::Topics),
            "notes" => Some(DrawerKind::Notes),
            "activity" => Some(DrawerKind::Activity),
            _ => None,
        }
    }
}

/// What the sidebar asks the pane to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarAction {
    /// A tag was selected or deselected.
    ToggleTag(i64),
    /// A topic was selected or deselected — one at a time.
    ToggleTopic(String),
    /// The AND/OR toggle was pressed.
    ToggleMode,
    /// The filter's selection was cleared — every tag and the topic.
    ClearFilter,
    /// A drawer was opened or closed.
    ToggleDrawer(DrawerKind),
    /// A noted subject was clicked: its tab opens — a catalog row or a
    /// local entry.
    OpenNote(NoteSubject),
    /// The editor was closed.
    CloseEditor,
    /// A new tag was typed for the edited row.
    AddTag { name: String },
    /// An existing tag was attached to the edited row.
    AttachTag(String),
    /// One of the edited row's tags was removed.
    RemoveTag(i64),
    /// The delete of a tag was armed: the count is shown, and the next
    /// press on the same tag deletes it.
    ArmDelete(i64),
    /// The armed delete was confirmed.
    ConfirmDelete(i64),
    /// The armed delete was cancelled, in the row.
    CancelDelete,
    /// The merge of a tag was armed: the next merge press names the
    /// tag it merges into.
    ArmMerge(i64),
    /// The armed merge was confirmed: the source merges into the target.
    MergeInto { source: i64, target: i64 },
    /// A rename was armed: the tag's name is edited in place.
    ArmRename(i64),
    /// The rename was confirmed.
    Rename { tag_id: i64, name: String },
}

/// One tag's id and name, as the editor speaks them.
pub(crate) type EditorAnswer = (Vec<(i64, String)>, Vec<(i64, String)>);

/// The tag editor's answer: the edited row's own tags, and the tags it
/// could carry.
#[derive(Debug)]
pub(crate) struct EditorTags {
    pub(crate) carried: Vec<(i64, String)>,
    pub(crate) available: Vec<(i64, String)>,
}

/// The sidebar's state: the drawer lists, the filter's selection, and
/// the row whose tags are being edited.
#[derive(Debug)]
pub(crate) struct SidebarState {
    tags: Vec<TagRecord>,
    topics: Vec<NameCount>,
    notes: Vec<NoteRecord>,
    selected: Vec<i64>,
    topic: Option<String>,
    match_mode: TagMatch,
    /// The one drawer that is open, when one is.
    open_drawer: Option<DrawerKind>,
    /// The filter box's text, lowercased — the word that narrows the
    /// tags, the topics and the notes at once.
    filter: String,
    edited: Option<i64>,
    /// The tag whose delete is waiting for its confirmation.
    armed_delete: Option<i64>,
    /// The tag that is about to be merged into another one.
    armed_merge: Option<i64>,
    /// The tag whose name is being edited.
    renaming: Option<i64>,
    /// The editor's cached answer: the edited row's tags and the ones
    /// it could carry. Fetched off the render path when the editor
    /// opens and after every editor action — never per frame.
    editor_tags: Option<EditorTags>,
    /// The last thing the sidebar did, said in one line.
    notice: String,
    /// The agent activity drawer's rows, as ready display lines.
    activity: Vec<String>,
    /// The presence line above them — what the newest heartbeat says.
    presence: Option<String>,
}

impl SidebarState {
    /// The sidebar's state with nothing in it.
    pub(crate) fn new() -> Self {
        Self {
            tags: Vec::new(),
            topics: Vec::new(),
            notes: Vec::new(),
            selected: Vec::new(),
            topic: None,
            match_mode: TagMatch::All,
            open_drawer: Some(DrawerKind::Tags),
            filter: String::new(),
            edited: None,
            armed_delete: None,
            armed_merge: None,
            renaming: None,
            editor_tags: None,
            notice: String::new(),
            activity: Vec::new(),
            presence: None,
        }
    }

    /// Replaces the tag list — the counts change whenever a tag moves.
    pub(crate) fn set_tags(&mut self, tags: Vec<TagRecord>) {
        self.tags = tags;
    }

    /// Replaces the topic list — the catalog's own aggregate.
    pub(crate) fn set_topics(&mut self, topics: Vec<NameCount>) {
        self.topics = topics;
    }

    /// Replaces the notes list.
    pub(crate) fn set_notes(&mut self, notes: Vec<NoteRecord>) {
        self.notes = notes;
    }

    /// Replaces the agent activity: the presence line and the drawer's
    /// rows, already shaped for display.
    pub(crate) fn set_activity(&mut self, presence: Option<String>, rows: Vec<String>) {
        self.presence = presence;
        self.activity = rows;
    }

    /// What the newest heartbeat says, when one is there.
    pub(crate) fn presence(&self) -> Option<&str> {
        self.presence.as_deref()
    }

    /// The agent activity drawer's rows.
    pub(crate) fn activity(&self) -> &[String] {
        &self.activity
    }

    /// The plain tags.
    pub(crate) fn tags(&self) -> &[TagRecord] {
        &self.tags
    }

    /// The catalog's topics with their counts.
    pub(crate) fn topics(&self) -> &[NameCount] {
        &self.topics
    }

    /// The repositories that carry a note.
    pub(crate) fn notes(&self) -> &[NoteRecord] {
        &self.notes
    }

    /// The filter box's text, as the lists read it.
    pub(crate) fn filter_text(&self) -> &str {
        &self.filter
    }

    /// Replaces the filter box's text — the pane's field's way in.
    /// The word is lowercased once here; the lists match against it.
    pub(crate) fn set_filter(&mut self, text: &str) {
        self.filter = text.trim().to_lowercase();
    }

    /// Whether the tag is part of the filter.
    pub(crate) fn is_selected(&self, tag_id: i64) -> bool {
        self.selected.contains(&tag_id)
    }

    /// Selects or deselects a tag; returns its new state.
    pub(crate) fn toggle(&mut self, tag_id: i64) -> bool {
        match self.selected.iter().position(|id| *id == tag_id) {
            Some(position) => {
                self.selected.remove(position);
                false
            }
            None => {
                self.selected.push(tag_id);
                true
            }
        }
    }

    /// The selected topic, when one narrows the library.
    pub(crate) fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    /// Selects or deselects the topic — one at a time: a second topic
    /// replaces the first, and the same topic again clears it.
    pub(crate) fn toggle_topic(&mut self, topic: &str) -> bool {
        let same = self.topic.as_deref() == Some(topic);
        self.topic = if same { None } else { Some(topic.to_string()) };
        !same
    }

    /// Whether the filter narrows anything.
    pub(crate) fn has_selection(&self) -> bool {
        !self.selected.is_empty() || self.topic.is_some()
    }

    /// How many filters are active — tags and the topic together. The
    /// number the clear-all says it dropped and the filter row shows.
    pub(crate) fn active_count(&self) -> usize {
        self.selected.len() + usize::from(self.topic.is_some())
    }

    /// The filter row's summary of what is active, in words: "3 tags ·
    /// topic: rust", or nothing when the filter is open wide.
    pub(crate) fn active_summary(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let mut pieces: Vec<String> = Vec::new();
        if !self.selected.is_empty() {
            pieces.push(format!("{} tag(s)", self.selected.len()));
        }
        if let Some(topic) = self.topic.as_deref() {
            pieces.push(format!("topic: {topic}"));
        }
        Some(pieces.join(" · "))
    }

    /// How the selected tags combine.
    pub(crate) fn match_mode(&self) -> TagMatch {
        self.match_mode
    }

    /// The tag whose delete waits for a confirmation.
    pub(crate) fn armed_delete(&self) -> Option<i64> {
        self.armed_delete
    }

    /// The tag that is about to be merged into another one.
    pub(crate) fn armed_merge(&self) -> Option<i64> {
        self.armed_merge
    }

    /// The tag whose name is being edited.
    pub(crate) fn renaming(&self) -> Option<i64> {
        self.renaming
    }

    /// Arms a delete: the tag's count is shown, and the next press on
    /// the same tag deletes it. Arming another tag replaces the arm.
    pub(crate) fn arm_delete(&mut self, tag_id: i64, count: u64) {
        self.armed_delete = Some(tag_id);
        self.armed_merge = None;
        self.renaming = None;
        self.notice = format!("delete this tag? {count} repositories carry it");
    }

    /// Arms a merge: the source waits for the tag it merges into.
    pub(crate) fn arm_merge(&mut self, tag_id: i64, name: &str, count: u64) {
        self.armed_merge = Some(tag_id);
        self.armed_delete = None;
        self.renaming = None;
        self.notice = format!("merge '{name}' into another tag — it carries {count} repositories");
    }

    /// Arms a rename: the tag's name is edited in place.
    pub(crate) fn arm_rename(&mut self, tag_id: i64) {
        self.renaming = Some(tag_id);
        self.armed_delete = None;
        self.armed_merge = None;
        self.notice = String::new();
    }

    /// Drops every armed operation and says what happened.
    pub(crate) fn clear_arms(&mut self, notice: &str) {
        self.armed_delete = None;
        self.armed_merge = None;
        self.renaming = None;
        self.notice = notice.to_string();
    }

    /// Whether any edit is armed — the click-outside rule's cue.
    pub(crate) fn has_arms(&self) -> bool {
        self.armed_delete.is_some() || self.armed_merge.is_some() || self.renaming.is_some()
    }

    /// The sidebar's last message.
    pub(crate) fn notice(&self) -> &str {
        &self.notice
    }

    /// Empties the selection — every tag and the topic — and returns
    /// how many filters it dropped.
    pub(crate) fn clear_selection(&mut self) -> usize {
        let dropped = self.active_count();
        self.selected.clear();
        self.topic = None;
        dropped
    }

    /// Flips the AND/OR toggle.
    pub(crate) fn toggle_mode(&mut self) {
        self.match_mode = match self.match_mode {
            TagMatch::All => TagMatch::Any,
            TagMatch::Any => TagMatch::All,
        };
    }

    /// The filter the page query honors.
    pub(crate) fn filter(&self) -> CatalogFilter {
        CatalogFilter {
            tag_ids: self.selected.clone(),
            tag_match: self.match_mode,
            topic: self.topic.clone(),
        }
    }

    /// The one drawer that is open, when one is.
    pub(crate) fn open_drawer(&self) -> Option<DrawerKind> {
        self.open_drawer
    }

    /// Opens a drawer, or closes it when it is the open one — never
    /// two at once.
    pub(crate) fn toggle_drawer(&mut self, drawer: DrawerKind) {
        self.open_drawer = if self.open_drawer == Some(drawer) {
            None
        } else {
            Some(drawer)
        };
    }

    /// Restores the persisted open drawer — the launch path's way in.
    pub(crate) fn restore_drawer(&mut self, name: Option<&str>) {
        self.open_drawer = name.and_then(DrawerKind::parse);
    }

    /// The row whose tags are being edited.
    pub(crate) fn edited(&self) -> Option<i64> {
        self.edited
    }

    /// Opens the editor for one row. The cached tag answer empties —
    /// it belongs to the row that was edited before.
    pub(crate) fn edit(&mut self, repo_rowid: i64) {
        self.edited = Some(repo_rowid);
        self.editor_tags = None;
    }

    /// The editor's cached answer, when it has landed.
    pub(crate) fn editor_tags(&self) -> Option<&EditorTags> {
        self.editor_tags.as_ref()
    }

    /// Lands the editor's answer.
    pub(crate) fn set_editor_tags(&mut self, tags: EditorTags) {
        self.editor_tags = Some(tags);
    }

    /// Closes the editor.
    pub(crate) fn close_editor(&mut self) {
        self.edited = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: i64, name: &str, repos: u64) -> TagRecord {
        TagRecord {
            id,
            name: name.to_string(),
            kind: yagna_store::TagKind::Tag,
            repos,
        }
    }

    fn note(id: i64, name: &str, text: &str) -> NoteRecord {
        NoteRecord {
            subject: yagna_core::model::NoteSubject::Repo(id),
            repo_ref: Some(yagna_core::model::RepoRef {
                forge: yagna_core::model::Forge::GitHub,
                owner: "someone".to_string(),
                name: name.to_string(),
            }),
            local_name: None,
            local_path: None,
            note: text.to_string(),
        }
    }

    #[test]
    fn a_toggle_selects_and_deselects() {
        let mut state = SidebarState::new();
        assert!(state.toggle(7));
        assert!(state.is_selected(7));
        assert!(state.has_selection());
        assert!(!state.toggle(7));
        assert!(!state.is_selected(7));
        assert!(!state.has_selection());
    }

    #[test]
    fn the_filter_carries_the_selection_the_mode_and_the_topic() {
        let mut state = SidebarState::new();
        state.toggle(1);
        state.toggle(2);
        state.toggle_topic("rust");
        let filter = state.filter();
        assert_eq!(filter.tag_ids, [1, 2]);
        assert_eq!(filter.tag_match, TagMatch::All, "AND is the default");
        assert_eq!(filter.topic.as_deref(), Some("rust"));
        state.toggle_mode();
        assert_eq!(state.filter().tag_match, TagMatch::Any);
    }

    #[test]
    fn a_topic_replaces_the_one_before_and_clears_on_a_repeat() {
        let mut state = SidebarState::new();
        assert!(state.toggle_topic("rust"), "the first pick selects");
        assert!(state.toggle_topic("cli"), "the second replaces");
        assert_eq!(state.topic.as_deref(), Some("cli"));
        assert!(!state.toggle_topic("cli"), "the repeat clears");
        assert_eq!(state.topic, None);
    }

    #[test]
    fn the_clear_all_drops_every_tag_and_the_topic_and_says_how_many() {
        let mut state = SidebarState::new();
        state.toggle(1);
        state.toggle(2);
        state.toggle(3);
        state.toggle_topic("rust");
        assert_eq!(state.active_count(), 4);
        assert_eq!(
            state.active_summary().expect("a summary"),
            "3 tag(s) · topic: rust"
        );
        assert_eq!(state.clear_selection(), 4, "four filters were dropped");
        assert!(!state.has_selection());
        assert_eq!(state.active_summary(), None);
    }

    #[test]
    fn one_drawer_is_open_at_a_time_and_the_state_round_trips_by_name() {
        let mut state = SidebarState::new();
        assert_eq!(state.open_drawer(), Some(DrawerKind::Tags));
        state.toggle_drawer(DrawerKind::Topics);
        assert_eq!(state.open_drawer(), Some(DrawerKind::Topics));
        state.toggle_drawer(DrawerKind::Notes);
        assert_eq!(
            state.open_drawer(),
            Some(DrawerKind::Notes),
            "opening one closes the other"
        );
        state.toggle_drawer(DrawerKind::Notes);
        assert_eq!(state.open_drawer(), None, "the repeat closes it");

        let name = DrawerKind::ALL
            .iter()
            .map(|drawer| drawer.name())
            .find(|name| DrawerKind::parse(name).is_some())
            .expect("every name parses");
        let mut restored = SidebarState::new();
        restored.restore_drawer(Some(name));
        assert_eq!(restored.open_drawer(), DrawerKind::parse(name));
        restored.restore_drawer(Some("nonsense"));
        assert_eq!(restored.open_drawer(), None);
        restored.restore_drawer(None);
        assert_eq!(restored.open_drawer(), None);
    }

    #[test]
    fn the_editor_opens_and_closes() {
        let mut state = SidebarState::new();
        assert_eq!(state.edited(), None);
        state.edit(42);
        assert_eq!(state.edited(), Some(42));
        state.close_editor();
        assert_eq!(state.edited(), None);
    }

    #[test]
    fn the_arms_replace_each_other() {
        let mut state = SidebarState::new();
        state.arm_delete(1, 34);
        assert_eq!(state.armed_delete(), Some(1));
        assert!(state.notice().contains("34"));
        assert!(state.has_arms());
        state.arm_merge(2, "rust", 12);
        assert_eq!(state.armed_delete(), None, "the delete arm is dropped");
        assert_eq!(state.armed_merge(), Some(2));
        state.arm_rename(3);
        assert_eq!(state.armed_merge(), None, "the merge arm is dropped");
        assert_eq!(state.renaming(), Some(3));
        state.clear_arms("merged");
        assert_eq!(state.renaming(), None);
        assert_eq!(state.notice(), "merged");
        assert!(!state.has_arms());
    }

    #[test]
    fn the_lists_are_kept_apart() {
        let mut state = SidebarState::new();
        state.set_tags(vec![tag(1, "rust", 3)]);
        state.set_topics(vec![NameCount {
            name: "cli".to_string(),
            count: 9,
        }]);
        state.set_notes(vec![note(7, "thing", "read this")]);
        assert_eq!(state.tags().len(), 1);
        assert_eq!(state.topics().len(), 1);
        assert_eq!(state.notes().len(), 1);
    }
}
