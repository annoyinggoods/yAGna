-- Schema version 1: the catalog. One row per repository the user has
-- ever starred (missing_at marks the gone ones — no deletes), plus the
-- sync_runs ledger. Multi-forge from day one: the (forge, owner, name)
-- key is the identity everywhere.
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE repos (
  id INTEGER PRIMARY KEY,
  forge TEXT NOT NULL DEFAULT 'github',
  repo_id INTEGER,
  owner TEXT NOT NULL, name TEXT NOT NULL,
  description TEXT,
  stars INTEGER NOT NULL DEFAULT 0,
  primary_language TEXT,
  languages_json TEXT,
  license_name TEXT, license_spdx TEXT,
  topics_json TEXT,
  default_branch TEXT,
  created_at TEXT, starred_at TEXT, last_commit_at TEXT,
  first_commit_date TEXT, first_commit_source TEXT NOT NULL DEFAULT 'repo-created',
  commit_count INTEGER NOT NULL DEFAULT 0,
  latest_release_tag TEXT, latest_release_at TEXT,
  is_fork INTEGER NOT NULL DEFAULT 0, is_archived INTEGER NOT NULL DEFAULT 0,
  health_score INTEGER, health_json TEXT,
  missing_at TEXT,
  first_seen_at TEXT NOT NULL, last_synced_at TEXT NOT NULL,
  UNIQUE (forge, owner, name)
);

CREATE TABLE sync_runs (
  id INTEGER PRIMARY KEY, started_at TEXT NOT NULL, finished_at TEXT, kind TEXT NOT NULL,
  pages INTEGER, repos_seen INTEGER, repos_upserted INTEGER,
  repos_new INTEGER, repos_missing INTEGER, error TEXT
);

CREATE INDEX idx_repos_starred ON repos(starred_at);
CREATE INDEX idx_repos_stars ON repos(stars);
CREATE INDEX idx_repos_health ON repos(health_score);
CREATE INDEX idx_repos_last_commit ON repos(last_commit_at);
CREATE INDEX idx_repos_name ON repos(name);
