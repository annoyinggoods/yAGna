-- Schema version 9: the README-to-asset mapping table.
-- Membership used to be answered by instr(rewritten,
-- 'yagna-asset://' || asset_id) -- an unindexable scan of every stored
-- README per asset row, on the list-preview and tab-open paths. The
-- mapping table answers the same question by primary key; the store
-- maintains the rows inside store_readme's own transaction from now
-- on, and the one-time backfill below extracts the ids straight out of
-- each stored body -- once per occurrence, not once per asset pair
-- (the naive readmes-x-assets join is tens of millions of instr calls
-- on a real catalog; this walk is tens of thousands).
-- `position` is the first-appearance order in the rewritten README,
-- which is what the preview and the tab's asset list sort by.
CREATE TABLE readme_assets (
  repo_rowid INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  asset_id   TEXT    NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
  position   INTEGER NOT NULL,
  PRIMARY KEY (repo_rowid, asset_id)
);
INSERT OR IGNORE INTO readme_assets (repo_rowid, asset_id, position)
WITH RECURSIVE
-- One README's asset references, walked hit by hit: `base` is how far
-- the search has consumed, `raw` is instr's answer inside the rest.
-- A row with raw = 0 (or NULL) has no further hits; its arm stops.
scan(repo_rowid, base, raw) AS (
  SELECT repo_rowid, 0, instr(rewritten, 'yagna-asset://') FROM readmes
  UNION ALL
  SELECT repo_rowid,
         base + raw,
         instr(substr(
           (SELECT rewritten FROM readmes WHERE repo_rowid = scan.repo_rowid),
           base + raw + 1),
           'yagna-asset://')
  FROM scan WHERE raw > 0
),
-- Every hit with its absolute offset and the 64-hex id behind the
-- scheme prefix.
hits(repo_rowid, hit, asset_id) AS (
  SELECT repo_rowid, base + raw,
         substr(
           (SELECT rewritten FROM readmes WHERE repo_rowid = scan.repo_rowid),
           base + raw + 14, 64)
  FROM scan WHERE raw > 0
)
SELECT h.repo_rowid, a.asset_id, h.hit
  FROM hits h JOIN assets a ON a.asset_id = h.asset_id
 ORDER BY h.repo_rowid, h.hit;
