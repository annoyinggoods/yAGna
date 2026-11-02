-- Schema version 4: the asset queue. Part03 seeded the asset inventory
-- (one row per unique image URL a README carries); this adds the queue
-- that fetches those bytes into the cache. The queue is asset-scoped, not
-- repository-scoped, so it gets its own table: one row per asset that
-- still wants its bytes, with the same resume-by-state machinery the
-- repository queue uses (pending | done | failed, an attempt counter, an
-- error note).
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

CREATE TABLE asset_queue (
  id INTEGER PRIMARY KEY,
  asset_id TEXT NOT NULL UNIQUE REFERENCES assets(asset_id),
  state TEXT NOT NULL DEFAULT 'pending',   -- pending | done | failed
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT, updated_at TEXT NOT NULL
);

CREATE INDEX idx_asset_queue_pending ON asset_queue(state, id);
