//! The user's tags: the table, the verbs that change it, and the counts
//! the sidebar filters by.
//!
//! A tag is one row in `tags` (a name, unique case-insensitively, and a
//! kind). A category is a tag by kind — one table, two kinds, no second
//! feature. The attachment table `repo_tags` carries the many-to-many.
//! Every verb here is idempotent where it can be: assigning a tag the
//! repository already carries changes nothing, and unassigning a tag it
//! does not carry is not an error.
//!
//! Items: `TagKind` — tag | category; `TagRecord` — one tag with the
//! number of repositories carrying it.
//! Core functions: `assign_tag` — create-on-assign; `unassign_tag`;
//! `rename_tag`; `merge_tags`; `delete_tag`; `tag_ref_count` — the count
//! reported before a merge or a delete; `tags_with_counts` — the
//! sidebar's list; `repo_tags` — one repository's tags.
//! Depends on: rusqlite, chrono.
//! Called by: the app's row chips and its sidebar.

use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::{Store, StoreError, saturating_rowid};

/// What a tag is: a plain tag, or a category (a tag the user treats as a
/// shelf). One table, two kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Tag,
    Category,
}

impl TagKind {
    /// The stored kind value.
    pub fn as_str(self) -> &'static str {
        match self {
            TagKind::Tag => "tag",
            TagKind::Category => "category",
        }
    }

    /// The stored kind value back into a kind; unknown text is none.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "tag" => Some(TagKind::Tag),
            "category" => Some(TagKind::Category),
            _ => None,
        }
    }
}

/// One tag with the number of repositories carrying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRecord {
    pub id: i64,
    pub name: String,
    pub kind: TagKind,
    /// How many repositories carry this tag.
    pub repos: u64,
}

impl Store {
    /// Assigns a tag to a repository, creating the tag when it does not
    /// exist yet. The name is matched case-insensitively (the table's
    /// collation), so "Rust" and "rust" are one tag. Returns the tag's
    /// id.
    pub fn assign_tag(
        &mut self,
        repo_rowid: u64,
        name: &str,
        kind: Option<TagKind>,
    ) -> Result<i64, StoreError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Parse("a tag needs a name".to_string()));
        }
        let transaction = self.conn.transaction()?;
        match kind {
            // The reader named a kind: the tag takes it, whether it is
            // new or an old plain tag.
            Some(kind) => {
                transaction.execute(
                    "INSERT INTO tags (name, kind) VALUES (?1, ?2)
                     ON CONFLICT(name) DO UPDATE SET kind = excluded.kind",
                    params![name, kind.as_str()],
                )?;
            }
            // The reader attached an existing tag: it keeps its kind.
            None => {
                transaction.execute(
                    "INSERT OR IGNORE INTO tags (name, kind) VALUES (?1, 'tag')",
                    params![name],
                )?;
            }
        }
        let tag_id: i64 = transaction.query_row(
            "SELECT id FROM tags WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO repo_tags (repo_rowid, tag_id) VALUES (?1, ?2)",
            params![saturating_rowid(repo_rowid), tag_id],
        )?;
        transaction.commit()?;
        Ok(tag_id)
    }

    /// Removes a tag from a repository. Returns whether the repository
    /// carried it at all.
    pub fn unassign_tag(&mut self, repo_rowid: u64, tag_id: i64) -> Result<bool, StoreError> {
        let removed = self.conn.execute(
            "DELETE FROM repo_tags WHERE repo_rowid = ?1 AND tag_id = ?2",
            params![saturating_rowid(repo_rowid), tag_id],
        )?;
        Ok(removed > 0)
    }

    /// Renames a tag. Every attachment follows, because the attachments
    /// point at the row, not at the name. A name another tag already
    /// carries is refused by the table's unique constraint.
    pub fn rename_tag(&mut self, tag_id: i64, name: &str) -> Result<(), StoreError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Parse("a tag needs a name".to_string()));
        }
        self.conn.execute(
            "UPDATE tags SET name = ?2 WHERE id = ?1",
            params![tag_id, name],
        )?;
        Ok(())
    }

    /// Merges one tag into another: the attachments move, the source tag
    /// goes away. Returns how many repositories now carry the target.
    pub fn merge_tags(&mut self, from_id: i64, into_id: i64) -> Result<u64, StoreError> {
        if from_id == into_id {
            return Err(StoreError::Parse(
                "a tag cannot be merged into itself".to_string(),
            ));
        }
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO repo_tags (repo_rowid, tag_id)
             SELECT repo_rowid, ?2 FROM repo_tags WHERE tag_id = ?1",
            params![from_id, into_id],
        )?;
        transaction.execute("DELETE FROM repo_tags WHERE tag_id = ?1", params![from_id])?;
        transaction.execute("DELETE FROM tags WHERE id = ?1", params![from_id])?;
        let count = transaction.query_row(
            "SELECT COUNT(*) FROM repo_tags WHERE tag_id = ?1",
            params![into_id],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.commit()?;
        Ok(count.max(0) as u64)
    }

    /// Deletes a tag and every attachment to it. Returns how many
    /// repositories lost it.
    pub fn delete_tag(&mut self, tag_id: i64) -> Result<u64, StoreError> {
        let transaction = self.conn.transaction()?;
        let count =
            transaction.execute("DELETE FROM repo_tags WHERE tag_id = ?1", params![tag_id])?;
        transaction.execute("DELETE FROM tags WHERE id = ?1", params![tag_id])?;
        transaction.commit()?;
        Ok(count as u64)
    }

    /// How many repositories carry a tag — the count the merge and delete
    /// confirmations report before they act.
    pub fn tag_ref_count(&self, tag_id: i64) -> Result<u64, StoreError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM repo_tags WHERE tag_id = ?1",
            params![tag_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Every tag with its repository count, name-sorted; the sidebar's
    /// list. `kind` narrows the list to tags or to categories.
    pub fn tags_with_counts(&self, kind: Option<TagKind>) -> Result<Vec<TagRecord>, StoreError> {
        let sql = "SELECT t.id, t.name, t.kind, COUNT(rt.repo_rowid)
                     FROM tags t
                     LEFT JOIN repo_tags rt ON rt.tag_id = t.id
                    WHERE (?1 IS NULL OR t.kind = ?1)
                    GROUP BY t.id, t.name, t.kind
                    ORDER BY t.name COLLATE NOCASE ASC";
        let mut statement = self.conn.prepare(sql)?;
        let kind_text = kind.map(TagKind::as_str);
        let rows = statement.query_map(params![kind_text], map_tag_record)?;
        let mut tags = Vec::new();
        for row in rows {
            tags.push(row?);
        }
        Ok(tags)
    }

    /// One repository's tags with their ids — the row's chips.
    pub fn repo_tags(&self, repo_rowid: u64) -> Result<Vec<TagRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, t.name, t.kind, COUNT(rt.repo_rowid)
               FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
              WHERE rt.repo_rowid = ?1
              GROUP BY t.id, t.name, t.kind
              ORDER BY t.name COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map(params![saturating_rowid(repo_rowid)], map_tag_record)?;
        let mut tags = Vec::new();
        for row in rows {
            tags.push(row?);
        }
        Ok(tags)
    }

    /// The id of the tag with that name — the chip's removal path, which
    /// knows the name and needs the id.
    pub fn tag_id_by_name(&self, name: &str) -> Result<Option<i64>, StoreError> {
        let id = self
            .conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(id)
    }

    /// One repository carrying the tag — the row a chip's removal
    /// belongs to.
    pub fn repo_rowid_of_tag(&self, tag_id: i64) -> Result<i64, StoreError> {
        let rowid = self.conn.query_row(
            "SELECT repo_rowid FROM repo_tags WHERE tag_id = ?1 LIMIT 1",
            params![tag_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(rowid)
    }
}

/// Maps one tag row into a record. An unknown kind is a store error: the
/// table's check constraint should have refused it.
fn map_tag_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TagRecord> {
    let kind_text: String = row.get(2)?;
    let kind = TagKind::parse(&kind_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            format!("unknown tag kind: {kind_text}").into(),
        )
    })?;
    Ok(TagRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        kind,
        repos: row.get::<_, i64>(3)?.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;
    use chrono::{DateTime, Utc};

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    fn seed(store: &mut Store, count: usize) -> Vec<u64> {
        let snapshots: Vec<yagna_core::model::RepoSnapshot> = (0..count)
            .map(|index| crate::testing::sample_snapshot("someone", &format!("repo-{index}")))
            .collect();
        store
            .upsert_snapshots(&snapshots, now())
            .expect("seed the catalog");
        let mut statement = store
            .conn
            .prepare("SELECT id FROM repos ORDER BY id")
            .expect("prepare");
        let mut rows = statement.query([]).expect("query");
        let mut ids = Vec::new();
        while let Some(row) = rows.next().expect("row") {
            ids.push(row.get::<_, i64>(0).expect("id") as u64);
        }
        ids
    }

    /// The number of attachments pointing at a tag that does not exist —
    /// the orphan check the merge and delete must leave at zero.
    fn orphans(store: &Store) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM repo_tags rt
                  LEFT JOIN tags t ON t.id = rt.tag_id
                 WHERE t.id IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("orphan count")
    }

    #[test]
    fn assigning_a_tag_creates_it_and_attaches_it() {
        let (_scratch, mut store) = store("assign");
        let ids = seed(&mut store, 2);
        let tag = store
            .assign_tag(ids[0], "reading list", Some(TagKind::Tag))
            .expect("assign");
        assert!(tag > 0);
        let again = store
            .assign_tag(ids[1], "Reading List", Some(TagKind::Tag))
            .expect("assign again");
        assert_eq!(tag, again, "the name is matched case-insensitively");
        assert_eq!(store.tag_ref_count(tag).expect("count"), 2);
        assert_eq!(orphans(&store), 0);
    }

    #[test]
    fn assigning_the_same_tag_twice_changes_nothing() {
        let (_scratch, mut store) = store("assign-twice");
        let ids = seed(&mut store, 1);
        let first = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        let second = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        assert_eq!(first, second);
        assert_eq!(store.tag_ref_count(first).expect("count"), 1);
    }

    #[test]
    fn unassigning_removes_only_that_attachment() {
        let (_scratch, mut store) = store("unassign");
        let ids = seed(&mut store, 2);
        let tag = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[1], "rust", Some(TagKind::Tag))
            .expect("assign");
        assert!(store.unassign_tag(ids[0], tag).expect("unassign"));
        assert!(
            !store.unassign_tag(ids[0], tag).expect("unassign again"),
            "the second removal finds nothing"
        );
        assert_eq!(store.tag_ref_count(tag).expect("count"), 1);
    }

    #[test]
    fn renaming_a_tag_keeps_every_attachment() {
        let (_scratch, mut store) = store("rename");
        let ids = seed(&mut store, 2);
        let tag = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[1], "rust", Some(TagKind::Tag))
            .expect("assign");
        store.rename_tag(tag, "systems").expect("rename");
        assert_eq!(store.tag_ref_count(tag).expect("count"), 2);
        let names = store.repo_tags(ids[0]).expect("tags");
        assert_eq!(names[0].name, "systems");
    }

    #[test]
    fn merging_moves_the_attachments_and_drops_the_source() {
        let (_scratch, mut store) = store("merge");
        let ids = seed(&mut store, 3);
        let source = store
            .assign_tag(ids[0], "old", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[1], "old", Some(TagKind::Tag))
            .expect("assign");
        let target = store
            .assign_tag(ids[1], "new", Some(TagKind::Tag))
            .expect("assign");
        assert_eq!(store.tag_ref_count(source).expect("count"), 2);

        let merged = store.merge_tags(source, target).expect("merge");
        assert_eq!(merged, 2, "two repositories carry the target now");
        assert_eq!(store.tag_ref_count(source).expect("count"), 0);
        assert_eq!(orphans(&store), 0);
        let names: Vec<String> = store
            .tags_with_counts(None)
            .expect("tags")
            .into_iter()
            .map(|tag| tag.name)
            .collect();
        assert_eq!(names, ["new"], "the source tag is gone");
    }

    #[test]
    fn deleting_a_tag_leaves_no_orphans() {
        let (_scratch, mut store) = store("delete");
        let ids = seed(&mut store, 2);
        let tag = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[1], "rust", Some(TagKind::Tag))
            .expect("assign");
        assert_eq!(store.tag_ref_count(tag).expect("count"), 2);

        let removed = store.delete_tag(tag).expect("delete");
        assert_eq!(removed, 2, "two repositories lost the tag");
        assert_eq!(store.tag_ref_count(tag).expect("count"), 0);
        assert_eq!(orphans(&store), 0);
        assert!(
            store.tags_with_counts(None).expect("tags").is_empty(),
            "the tag row is gone"
        );
    }

    /// The kind of a tag, as the catalog holds it.
    fn kind_of(store: &Store, name: &str) -> TagKind {
        let kind: String = store
            .conn
            .query_row(
                "SELECT kind FROM tags WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .expect("the tag");
        TagKind::parse(&kind).expect("a known kind")
    }

    #[test]
    fn a_tag_becomes_a_category_when_the_reader_says_so() {
        let (_scratch, mut store) = store("kind");
        let ids = seed(&mut store, 2);
        store
            .assign_tag(ids[0], "reading", Some(TagKind::Tag))
            .expect("assign");
        assert_eq!(kind_of(&store, "reading"), TagKind::Tag);

        // The same name, named as a category: the reader's choice wins.
        store
            .assign_tag(ids[1], "reading", Some(TagKind::Category))
            .expect("assign");
        assert_eq!(
            kind_of(&store, "reading"),
            TagKind::Category,
            "the kind is the reader's choice, not the first one's"
        );

        // Attaching an existing tag keeps the kind it has.
        store.assign_tag(ids[0], "reading", None).expect("attach");
        assert_eq!(
            kind_of(&store, "reading"),
            TagKind::Category,
            "attaching does not change what a tag is"
        );
    }

    #[test]
    fn a_tag_cannot_be_merged_into_itself() {
        let (_scratch, mut store) = store("merge-self");
        let ids = seed(&mut store, 1);
        let tag = store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        assert!(store.merge_tags(tag, tag).is_err());
        assert_eq!(store.tag_ref_count(tag).expect("count"), 1);
    }

    #[test]
    fn the_sidebar_list_carries_the_counts() {
        let (_scratch, mut store) = store("counts");
        let ids = seed(&mut store, 3);
        store
            .assign_tag(ids[0], "rust", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[1], "rust", Some(TagKind::Tag))
            .expect("assign");
        store
            .assign_tag(ids[2], "books", Some(TagKind::Category))
            .expect("assign");

        let all = store.tags_with_counts(None).expect("tags");
        assert_eq!(all.len(), 2);
        let books = all
            .iter()
            .find(|tag| tag.name == "books")
            .expect("the category");
        assert_eq!(books.kind, TagKind::Category);
        assert_eq!(books.repos, 1);

        let tags_only = store.tags_with_counts(Some(TagKind::Tag)).expect("tags");
        assert_eq!(tags_only.len(), 1);
        assert_eq!(tags_only[0].repos, 2);
    }

    #[test]
    fn the_tags_survive_a_restart() {
        let (scratch, mut store) = store("restart");
        let ids = seed(&mut store, 2);
        let tag = store
            .assign_tag(ids[0], "reading list", Some(TagKind::Tag))
            .expect("assign");
        drop(store);

        let reopened = Store::open(&scratch.path()).expect("reopen");
        assert_eq!(reopened.tag_ref_count(tag).expect("count"), 1);
        let names = reopened.repo_tags(ids[0]).expect("tags");
        assert_eq!(names[0].name, "reading list");
        assert_eq!(reopened.tags_with_counts(None).expect("tags").len(), 1);
    }

    #[test]
    fn a_tag_needs_a_name() {
        let (_scratch, mut store) = store("no-name");
        let ids = seed(&mut store, 1);
        assert!(store.assign_tag(ids[0], "   ", Some(TagKind::Tag)).is_err());
        assert!(store.rename_tag(1, "").is_err());
    }
}
