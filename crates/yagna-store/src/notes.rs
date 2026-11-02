//! The notes: one repository's own words — the reader's sentence about
//! one repository, written by the reader and read back by the reader.
//!
//! A note is not a label: it has no name, no colour and no membership
//! list. It is the note of one subject and nothing else, so the table is
//! keyed by the subject's kind and the subject's id, and it holds one
//! text. The subjects are a catalog row and a local entry (a
//! repository that lives on this machine) — one table, one write path,
//! whichever the note is about.
//!
//! The sweep never writes here — the repos table and this one share
//! only the key — so a re-sync never overwrites what the reader wrote.
//! An empty note is no note: saving nothing deletes the row.
//!
//! Items: `NoteRecord` — one noted subject, with the note's first
//! words for the sidebar's drawer.
//! Core functions: `note_of` — one subject's note; `set_note` — write
//! or clear; `notes_with_names` — every noted subject.
//! Depends on: rusqlite, chrono, yagna_core (the note's subject),
//! crate::catalog (the timestamp format, the row id helper).
//! Called by: the app's repository tab (the note above the README),
//! its local tab, and the sidebar (the notes drawer).

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;
use yagna_core::model::{Forge, NoteSubject, RepoRef};

use crate::{Store, StoreError, catalog};

/// One noted subject: what it is, and the note it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRecord {
    /// The subject the note is about.
    pub subject: NoteSubject,
    /// The repository the note is about, when it is a catalog row.
    pub repo_ref: Option<RepoRef>,
    /// The local entry's name, when the note is about one.
    pub local_name: Option<String>,
    /// The local entry's path, when the note is about one.
    pub local_path: Option<String>,
    /// The note itself.
    pub note: String,
}

impl NoteRecord {
    /// The name the drawer shows: `owner/name` for a catalog row, the
    /// entry's own name for a local one.
    pub fn title(&self) -> String {
        match (&self.repo_ref, &self.local_name) {
            (Some(repo), _) => format!("{}/{}", repo.owner, repo.name),
            (None, Some(name)) => name.clone(),
            (None, None) => "a repository".to_string(),
        }
    }
}

impl Store {
    /// One subject's note, when it carries one.
    pub fn note_of(&self, subject: NoteSubject) -> Result<Option<String>, StoreError> {
        let note = self
            .conn
            .query_row(
                "SELECT note FROM notes WHERE subject_kind = ?1 AND subject_id = ?2",
                params![subject.kind(), subject.id()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(note)
    }

    /// Writes one subject's note. An empty note deletes the row —
    /// "no note" is a state the reader can return to, not a blank text
    /// that outlives its meaning. The note's length is the reader's:
    /// nothing here caps it.
    pub fn set_note(
        &mut self,
        subject: NoteSubject,
        note: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let trimmed = note.trim();
        if trimmed.is_empty() {
            self.conn.execute(
                "DELETE FROM notes WHERE subject_kind = ?1 AND subject_id = ?2",
                params![subject.kind(), subject.id()],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO notes (subject_kind, subject_id, note, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(subject_kind, subject_id) DO UPDATE SET
                   note = excluded.note, updated_at = excluded.updated_at",
            params![subject.kind(), subject.id(), trimmed, catalog::stamp(now)],
        )?;
        Ok(())
    }

    /// Every subject that carries a note, name-sorted — the notes
    /// drawer's list. Missing subjects are excluded: the drawer shows
    /// what the library can open.
    pub fn notes_with_names(&self) -> Result<Vec<NoteRecord>, StoreError> {
        let mut notes = self.catalog_notes()?;
        notes.extend(self.local_notes()?);
        notes.sort_by_key(|note| note.title().to_lowercase());
        Ok(notes)
    }

    /// The notes of the catalog's rows.
    fn catalog_notes(&self) -> Result<Vec<NoteRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT n.subject_id, r.forge, r.owner, r.name, n.note
               FROM notes n JOIN repos r ON r.id = n.subject_id
              WHERE n.subject_kind = 'repo' AND r.missing_at IS NULL",
        )?;
        let rows = statement.query_map([], map_catalog_note)?;
        let mut notes = Vec::new();
        for row in rows {
            notes.push(row?);
        }
        Ok(notes)
    }

    /// The notes of the local entries.
    fn local_notes(&self) -> Result<Vec<NoteRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT n.subject_id, l.name, l.path, n.note
               FROM notes n JOIN local_repos l ON l.id = n.subject_id
              WHERE n.subject_kind = 'local' AND l.missing_at IS NULL",
        )?;
        let rows = statement.query_map([], map_local_note)?;
        let mut notes = Vec::new();
        for row in rows {
            notes.push(row?);
        }
        Ok(notes)
    }
}

/// Maps one catalog note row into a record. An unknown forge is a store
/// error: the column was written by this store.
fn map_catalog_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRecord> {
    let forge_text: String = row.get(1)?;
    let forge = match forge_text.as_str() {
        "github" => Forge::GitHub,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown forge: {other}").into(),
            ));
        }
    };
    Ok(NoteRecord {
        subject: NoteSubject::Repo(row.get(0)?),
        repo_ref: Some(RepoRef {
            forge,
            owner: row.get(2)?,
            name: row.get(3)?,
        }),
        local_name: None,
        local_path: None,
        note: row.get(4)?,
    })
}

/// Maps one local note row into a record.
fn map_local_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRecord> {
    Ok(NoteRecord {
        subject: NoteSubject::Local(row.get(0)?),
        repo_ref: None,
        local_name: Some(row.get(1)?),
        local_path: Some(row.get(2)?),
        note: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    /// The row ids of the seeded repositories, in the order named.
    fn seed(store: &mut Store, names: &[&str]) -> Vec<u64> {
        let snapshots: Vec<_> = names
            .iter()
            .map(|name| crate::testing::sample_snapshot("someone", name))
            .collect();
        store.upsert_snapshots(&snapshots, now()).expect("seed");
        names
            .iter()
            .map(|name| {
                store
                    .conn
                    .query_row(
                        "SELECT id FROM repos WHERE name = ?1",
                        params![name],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("rowid") as u64
            })
            .collect()
    }

    #[test]
    fn a_note_round_trips_and_an_empty_note_is_no_note() {
        let (_scratch, mut store) = store("note-roundtrip");
        let id = seed(&mut store, &["noted"]).remove(0);
        let subject = NoteSubject::Repo(id as i64);
        assert_eq!(store.note_of(subject).expect("read"), None);

        store
            .set_note(subject, "  the parser I keep coming back to  ", now())
            .expect("write");
        assert_eq!(
            store.note_of(subject).expect("read"),
            Some("the parser I keep coming back to".to_string()),
            "the note is trimmed, never blank-padded"
        );

        store.set_note(subject, "   ", now()).expect("clear");
        assert_eq!(
            store.note_of(subject).expect("read"),
            None,
            "an empty note deletes the row"
        );
    }

    /// The note's length is the reader's: a long note is written whole,
    /// and read back whole. Nothing caps it.
    #[test]
    fn a_long_note_is_kept_whole() {
        let (_scratch, mut store) = store("note-long");
        let id = seed(&mut store, &["long"]).remove(0);
        let subject = NoteSubject::Repo(id as i64);
        let text = "a line of the reader's own words\n".repeat(2_000);
        store.set_note(subject, &text, now()).expect("write");
        let read = store.note_of(subject).expect("read").expect("a note");
        assert_eq!(read.len(), text.trim().len(), "no length is lost");
        assert!(read.contains('\n'), "a note may be a paragraph");
    }

    #[test]
    fn the_notes_list_names_the_repositories_that_carry_one() {
        let (_scratch, mut store) = store("note-list");
        let ids = seed(&mut store, &["alpha", "beta"]);
        store
            .set_note(
                NoteSubject::Repo(ids[0] as i64),
                "read this one first",
                now(),
            )
            .expect("write");
        store
            .set_note(NoteSubject::Repo(ids[1] as i64), "the tiny one", now())
            .expect("write");

        let notes = store.notes_with_names().expect("list");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].repo_ref.as_ref().expect("a repo").name, "alpha");
        assert_eq!(notes[1].subject, NoteSubject::Repo(ids[1] as i64));
        assert_eq!(notes[0].repo_ref.as_ref().expect("a repo").owner, "someone");
        assert_eq!(
            notes[0].repo_ref.as_ref().expect("a repo").forge,
            Forge::GitHub
        );
        assert!(notes[0].note.contains("read this"));
    }

    /// The note belongs to the repository, not to the forge: a re-sync's
    /// upsert of the same repository leaves the note standing.
    #[test]
    fn a_resync_never_overwrites_a_note() {
        let (_scratch, mut store) = store("note-sync");
        let id = seed(&mut store, &["kept"]).remove(0);
        store
            .set_note(NoteSubject::Repo(id as i64), "my words", now())
            .expect("write");

        let snapshots = vec![crate::testing::sample_snapshot("someone", "kept")];
        store.upsert_snapshots(&snapshots, now()).expect("re-sync");
        assert_eq!(
            store.note_of(NoteSubject::Repo(id as i64)).expect("read"),
            Some("my words".to_string()),
            "the sweep writes the repos table, never the notes"
        );
    }
}
