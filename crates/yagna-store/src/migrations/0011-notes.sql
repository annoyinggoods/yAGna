-- Schema version 11: the notes table. A note is one repository's own
-- sentence — one row per repository, written by the reader, never
-- touched by the sweep. Categories (schema 3's tags of kind
-- 'category') are repurposed by this migration: every repository that
-- carried one or more categories becomes the bearer of a note that
-- keeps their names ("unfiled: …"), and the category tags themselves
-- are deleted — GitHub's own topics replace them as the sidebar's
-- second axis. The choice is named here so it can be found: the
-- categories survive as notes on the repositories that carried them,
-- never as a group of their own.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE notes (
  repo_rowid INTEGER PRIMARY KEY REFERENCES repos(id),
  note TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

INSERT INTO notes (repo_rowid, note, updated_at)
SELECT rt.repo_rowid,
       'unfiled: ' || COALESCE((
         SELECT group_concat(t.name, ', ')
           FROM repo_tags rt2 JOIN tags t ON t.id = rt2.tag_id
          WHERE rt2.repo_rowid = rt.repo_rowid AND t.kind = 'category'
       ), ''),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
  FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
 WHERE t.kind = 'category'
 GROUP BY rt.repo_rowid;

DELETE FROM repo_tags
 WHERE tag_id IN (SELECT id FROM tags WHERE kind = 'category');
DELETE FROM tags WHERE kind = 'category';
