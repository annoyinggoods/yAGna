-- Schema version 3: user tags. One row per unique tag name (categories
-- are tags by kind — one table, two kinds), and one row per repository-
-- tag attachment. The read side's page query joins these to fold the
-- user's tags into every row it serves.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE tags (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  kind TEXT NOT NULL CHECK (kind IN ('tag','category'))
);

CREATE TABLE repo_tags (
  repo_rowid INTEGER NOT NULL REFERENCES repos(id),
  tag_id INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (repo_rowid, tag_id)
);

CREATE INDEX idx_repo_tags_tag ON repo_tags(tag_id);
