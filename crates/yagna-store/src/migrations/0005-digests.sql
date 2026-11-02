-- Schema version 5: digests and the full-text indexes. The digests
-- table is the derived artifact, never the catalog's truth: one row per
-- repository whose digest was taken (the gitingest-format body, the
-- spec it was taken under, and the branch head it was cut from), with
-- `stale_at` stamped by the sweep when the repository's last commit
-- moved — a stale digest is regenerated on the next open, never read.
-- The two FTS5 tables are external content over the stored originals
-- (the README's original text, the digest's body): the index carries
-- no copy of the text, and the store keeps the index rows in step
-- inside the same transaction that writes the body. The FTS columns
-- name the content tables' own columns (readmes.original,
-- digests.body); the rewritten README (asset ids in place of image
-- urls) is deliberately never indexed.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE digests (
  repo_rowid INTEGER PRIMARY KEY REFERENCES repos(id),
  spec_json TEXT NOT NULL,
  body TEXT NOT NULL,
  token_estimate INTEGER,
  truncated INTEGER NOT NULL DEFAULT 0,
  taken_at TEXT NOT NULL,
  taken_commit TEXT,
  stale_at TEXT
);

CREATE VIRTUAL TABLE fts_readme USING fts5(
  original, content='readmes', content_rowid='repo_rowid', tokenize='unicode61'
);
CREATE VIRTUAL TABLE fts_digest USING fts5(
  body, content='digests', content_rowid='repo_rowid', tokenize='unicode61'
);

-- The index starts in step with what is already stored.
INSERT INTO fts_readme(rowid, original) SELECT repo_rowid, original FROM readmes;

CREATE INDEX idx_digests_stale ON digests(stale_at);
