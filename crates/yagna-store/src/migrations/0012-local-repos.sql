-- Schema version 12: the local repositories, and the note's subject.
-- A repository that lives on this machine is a fact about the disk, not
-- about the service, so it gets its own table: one row per worktree the
-- scan found, with the path it lives at, the origin its own config
-- names, and the catalog row it matches (`linked_rowid` is NULL when the
-- catalog has no row for it — the entry is then standalone, and says
-- so). `missing_at` marks a folder that went away: the row is never
-- deleted, because the reader may move the folder back.
-- The notes table is rebuilt here, because a note may belong to a
-- catalog row or to a local entry: the key becomes the subject's kind
-- and the subject's id, and the rows are copied before the old table is
-- dropped — inside this one transaction, so the copy either lands whole
-- or the old table stands.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE local_repos (
  id INTEGER PRIMARY KEY,
  root TEXT NOT NULL,
  path TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  owner TEXT,
  repo_name TEXT,
  origin TEXT,
  branch TEXT,
  last_commit_at TEXT,
  file_count INTEGER NOT NULL DEFAULT 0,
  total_bytes INTEGER NOT NULL DEFAULT 0,
  linked_rowid INTEGER,
  missing_at TEXT,
  scanned_at TEXT NOT NULL
);

CREATE INDEX idx_local_root ON local_repos(root);
CREATE INDEX idx_local_linked ON local_repos(linked_rowid);

CREATE TABLE notes_subjects (
  subject_kind TEXT NOT NULL CHECK (subject_kind IN ('repo','local')),
  subject_id INTEGER NOT NULL,
  note TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (subject_kind, subject_id)
);

INSERT INTO notes_subjects (subject_kind, subject_id, note, updated_at)
SELECT 'repo', repo_rowid, note, updated_at FROM notes;

DROP TABLE notes;
ALTER TABLE notes_subjects RENAME TO notes;
