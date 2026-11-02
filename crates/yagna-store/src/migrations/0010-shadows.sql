-- Schema version 10: the read-only shadows. Other users' starred
-- lists and the DISCOVER results are views of other people's
-- repositories: they never mix into the catalog, so they live in their
-- own table scoped by login — the distinction is structural, never a
-- flag. No tags, no health, no enrichment beyond what the row shows;
-- the catalog's queries never read this table. `followed_users` is the
-- PEOPLE tab's list with its last-fetch stamp; `shadow_repos` holds at
-- most 500 rows per login (views, not history).
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE shadow_repos (
  login TEXT NOT NULL,
  forge TEXT NOT NULL DEFAULT 'github',
  owner TEXT NOT NULL, name TEXT NOT NULL,
  description TEXT,
  stars INTEGER NOT NULL DEFAULT 0,
  primary_language TEXT,
  languages_json TEXT,
  license_name TEXT, license_spdx TEXT,
  topics_json TEXT,
  default_branch TEXT,
  created_at TEXT, last_commit_at TEXT,
  fetched_at TEXT NOT NULL,
  PRIMARY KEY (login, forge, owner, name)
);

CREATE INDEX idx_shadow_login ON shadow_repos(login);
CREATE INDEX idx_shadow_login_stars ON shadow_repos(login, stars DESC);

CREATE TABLE followed_users (
  login TEXT PRIMARY KEY,
  fetched_at TEXT
);
