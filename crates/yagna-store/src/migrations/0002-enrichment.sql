-- Schema version 2: the lazy layer. Per-repository enrichment (the
-- README with its offline rewrite, non-standard license texts, the
-- rendered tree), the asset inventory the rewrite seeds, and the
-- enrichment queue the serial runner works. One row per repository in
-- readmes/license_texts/trees (repo_rowid is the primary key); one row
-- per unique asset URL in assets (refcount counts the referencing
-- repositories); the queue is the resume point — a killed run loses
-- nothing, a re-run continues where it stopped.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE readmes (
  repo_rowid INTEGER PRIMARY KEY REFERENCES repos(id),
  original TEXT NOT NULL, rewritten TEXT NOT NULL,
  excerpt TEXT NOT NULL, truncated INTEGER NOT NULL DEFAULT 0,
  fetched_at TEXT NOT NULL
);

CREATE TABLE license_texts (
  repo_rowid INTEGER PRIMARY KEY REFERENCES repos(id),
  body TEXT NOT NULL, fetched_at TEXT NOT NULL
);

CREATE TABLE trees (
  repo_rowid INTEGER PRIMARY KEY REFERENCES repos(id),
  rendered TEXT NOT NULL, truncated INTEGER NOT NULL DEFAULT 0,
  fetched_at TEXT NOT NULL
);

CREATE TABLE assets (
  asset_id TEXT PRIMARY KEY, url TEXT NOT NULL,
  badge INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'missing',   -- missing | present | failed
  bytes INTEGER, path TEXT, fetched_at TEXT,
  refcount INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE enrichment_queue (
  id INTEGER PRIMARY KEY,
  repo_rowid INTEGER NOT NULL REFERENCES repos(id),
  kind TEXT NOT NULL,                      -- readme | license | tree
  state TEXT NOT NULL DEFAULT 'pending',   -- pending | done | failed | skipped
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT, updated_at TEXT NOT NULL
);

CREATE INDEX idx_queue_pending ON enrichment_queue(state, id);
