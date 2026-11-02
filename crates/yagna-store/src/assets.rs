//! The asset cache's landing side: the preview image of a repository,
//! the queue that fetches its bytes, and the README head the expanded
//! row shows.
//!
//! Part03 seeded the asset inventory — one row per unique image URL a
//! README carries, with a badge flag. This module is what the expanded
//! row asks for: the repository's *first non-badge* asset (the position
//! is the order the rewritten README mentions it in), its state, and the
//! path its bytes were cached under. A missing asset is enqueued for
//! fetching; the bytes land in `<cache>/<asset_id>.img` and the row's
//! `path` column points at the file, so the view reads the local file
//! and never the network. The queue is leased like the enrichment
//! queue: a claim marks the row and a lapsed lease re-queues it, so a
//! killed run loses nothing. The cache has a cap: `prune_cache` evicts
//! the oldest-fetched assets — never a pinned one, an open view's —
//! and then the oldest digests, until the cache fits again; evicted
//! rows go back to `missing` so the next look re-fetches them.
//!
//! Items: `AssetRecord` — one asset with its state and cached path;
//! `AssetJob` — one claimed queue row; `ReadmeHead` — the first lines
//! of a stored README; `CacheStats`/`PruneReport` — what the cap's
//! probe and its eviction report; `CacheBreakdown` — the per-kind byte
//! counts the stats line shows.
//! Core functions: `preview_asset` — the row's first non-badge asset;
//! `enqueue_asset`/`claim_asset`/`heartbeat_asset`/the stamps — the
//! queue; `store_asset_bytes` — the cache landing; `prune_cache` — the
//! cap; `cache_breakdown` — the per-kind counts; `readme_head` — the
//! preview text.
//! Depends on: rusqlite, chrono, std::fs (the cache file), crate::catalog
//! (the timestamp format), crate::digests (the digest side of the cap).
//! Called by: the app's expanded row, the enrichment runner, and the
//! stats verb.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::{Store, StoreError, catalog, saturating_rowid};

/// The file extension the cache keeps an asset's bytes under. The
/// content is sniffed when the image is decoded, so the extension is a
/// name, not a claim about the format.
pub const ASSET_EXTENSION: &str = "img";

/// The width a cached image is scaled down to, at most. A README's
/// screenshot is read at a few hundred pixels; the original is an
/// archive, not a picture.
pub const IMAGE_MAX_WIDTH: u32 = 400;

/// The height a cached image is scaled down to, at most.
pub const IMAGE_MAX_HEIGHT: u32 = 300;

/// One asset: its identity, its source URL, whether it is a badge, and
/// where its bytes are (or are not) cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRecord {
    pub asset_id: String,
    pub url: String,
    /// A badge is marked and never downloaded.
    pub badge: bool,
    /// `missing`, `present` or `failed`.
    pub state: String,
    /// The cached file, when the asset is present.
    pub path: Option<PathBuf>,
    /// The cached size in bytes, when the asset is present.
    pub bytes: Option<u64>,
}

/// One claimed asset job: the asset to fetch and where to get it. The
/// token is the claim's ownership — heartbeats and stamps carry it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetJob {
    pub id: u64,
    pub asset_id: String,
    pub url: String,
    pub claim_token: String,
}

/// The head of a stored README: the first lines, and whether more follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmeHead {
    /// The lines themselves, joined with newlines.
    pub text: String,
    /// How many lines the README has in total.
    pub total_lines: usize,
    /// Whether the README is longer than the head.
    pub truncated: bool,
}

impl Store {
    /// One asset by id, or `None` when the catalog never saw it.
    pub fn asset(&self, asset_id: &str) -> Result<Option<AssetRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT asset_id, url, badge, state, path, bytes FROM assets WHERE asset_id = ?1",
        )?;
        let mut rows = statement.query(params![asset_id])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(map_asset(row)?)),
        }
    }

    /// The first non-badge asset of one repository: the image the
    /// expanded row previews. `None` when the repository's README
    /// carries no image at all, or carries only badges. The mapping
    /// table answers membership by primary key — the `instr` scan it
    /// replaced cost a walk of every stored README per asset row.
    pub fn preview_asset(&self, repo_rowid: u64) -> Result<Option<AssetRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT a.asset_id, a.url, a.badge, a.state, a.path, a.bytes
               FROM readme_assets ra JOIN assets a ON a.asset_id = ra.asset_id
              WHERE ra.repo_rowid = ?1 AND a.badge = 0
              ORDER BY ra.position ASC LIMIT 1",
        )?;
        let mut rows = statement.query(params![saturating_rowid(repo_rowid)])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(Some(map_asset(row)?)),
        }
    }

    /// Enqueues one asset for fetching. Idempotent: an asset already
    /// queued (in any live state) is not queued again, and a failed row
    /// may re-enqueue — the next run is its retry.
    pub fn enqueue_asset(&mut self, asset_id: &str, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO asset_queue (asset_id, state, updated_at)
             SELECT ?1, 'pending', ?2
              WHERE EXISTS (SELECT 1 FROM assets WHERE asset_id = ?1)
                AND NOT EXISTS (
                  SELECT 1 FROM asset_queue
                   WHERE asset_id = ?1 AND state IN ('pending', 'claimed', 'done')
                )",
            params![asset_id, catalog::stamp(now)],
        )?;
        Ok(inserted as u64)
    }

    /// Enqueues every asset whose bytes are not cached yet — the bulk
    /// fill the enrichment runner uses.
    pub fn enqueue_missing_assets(&mut self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO asset_queue (asset_id, state, updated_at)
             SELECT a.asset_id, 'pending', ?1
               FROM assets a
              WHERE a.badge = 0 AND a.state != 'present'
                AND NOT EXISTS (
                  SELECT 1 FROM asset_queue q
                   WHERE q.asset_id = a.asset_id AND q.state IN ('pending', 'claimed', 'done')
                )",
            params![catalog::stamp(now)],
        )?;
        Ok(inserted as u64)
    }

    /// Claims the oldest eligible asset job under a lease, like the
    /// enrichment queue's claim: a live lease is not re-claimed, a
    /// lapsed one (its runner died) is. The claim names itself with an
    /// ownership token; the transaction is immediate, so two processes
    /// against one database serialize here.
    pub fn claim_asset(&self) -> Result<Option<AssetJob>, StoreError> {
        let now = Utc::now();
        let transaction = crate::ImmediateTx::begin(&self.conn)?;
        let id: Option<i64> = transaction
            .conn
            .query_row(
                "SELECT q.id FROM asset_queue q
                  WHERE (q.state = 'pending'
                         OR (q.state = 'claimed'
                             AND (q.lease_until IS NULL OR q.lease_until <= ?1)))
                    AND (q.not_before IS NULL OR q.not_before <= ?1)
                  ORDER BY q.id LIMIT 1",
                params![catalog::stamp(now)],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        transaction.conn.execute(
            "UPDATE asset_queue
             SET state = 'claimed', lease_until = ?2, updated_at = ?3,
                 claim_token = lower(hex(randomblob(16)))
              WHERE id = ?1",
            params![
                id,
                catalog::stamp(now + crate::enrichment::lease_duration()),
                catalog::stamp(now),
            ],
        )?;
        let job = read_job(transaction.conn, id)?;
        transaction.commit()?;
        Ok(Some(job))
    }

    /// Renews a claimed asset job's lease — the fetch loop's heartbeat.
    /// `false` means the token no longer owns the row; the caller lets
    /// go instead of fetching into another runner's claim.
    pub fn heartbeat_asset(
        &self,
        id: u64,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let renewed = self.conn.execute(
            "UPDATE asset_queue SET lease_until = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2 AND state = 'claimed'",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(now + crate::enrichment::lease_duration()),
                catalog::stamp(now),
            ],
        )?;
        Ok(renewed > 0)
    }

    /// Puts a claimed asset job back without burning an attempt:
    /// `pending` again under the retry backoff — the transport-failure
    /// path's release, so an offline session's row waits instead of
    /// hot-looping. A token that no longer owns the row releases
    /// nothing.
    pub fn release_asset_claim(
        &mut self,
        id: u64,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE asset_queue
             SET state = 'pending', lease_until = NULL, claim_token = NULL,
                 not_before = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2 AND state = 'claimed'",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(now + crate::enrichment::retry_backoff()),
                catalog::stamp(now),
            ],
        )?;
        Ok(())
    }

    /// Parks a rate-limited asset job: back to `pending` (no attempt
    /// burned), invisible to claims until `until` passes.
    pub fn stamp_asset_rate_limited(
        &mut self,
        id: u64,
        token: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE asset_queue
             SET state = 'pending', lease_until = NULL, claim_token = NULL,
                 not_before = ?3, updated_at = ?4
              WHERE id = ?1 AND claim_token = ?2",
            params![
                saturating_rowid(id),
                token,
                catalog::stamp(until),
                catalog::stamp(now)
            ],
        )?;
        Ok(())
    }

    /// The earliest `not_before` among the asset queue's waiting rows —
    /// the visible pause the fetch loop counts down to.
    pub fn asset_backoff_until(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let text: Option<String> = self.conn.query_row(
            "SELECT MIN(not_before) FROM asset_queue
              WHERE not_before IS NOT NULL AND state = 'pending'",
            [],
            |row| row.get(0),
        )?;
        match text {
            Some(text) => Ok(Some(catalog::parse_stamp(&text)?)),
            None => Ok(None),
        }
    }

    /// Stamps a claimed asset job done. Token-guarded like every stamp.
    pub fn stamp_asset_done(
        &mut self,
        id: u64,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE asset_queue
             SET state = 'done', last_error = NULL, lease_until = NULL, not_before = NULL,
                 claim_token = NULL, updated_at = ?3
              WHERE id = ?1 AND claim_token = ?2",
            params![saturating_rowid(id), token, catalog::stamp(now)],
        )?;
        Ok(())
    }

    /// Stamps a claimed asset job failed, with the reason recorded. The
    /// attempt counter is the queue's own: three strikes fail it for
    /// real; before that the row waits out the retry backoff.
    pub fn stamp_asset_failed(
        &mut self,
        id: u64,
        token: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE asset_queue SET attempts = attempts + 1,
                    state = CASE WHEN attempts + 1 >= ?3 THEN 'failed' ELSE 'pending' END,
                    lease_until = NULL,
                    claim_token = NULL,
                    not_before = CASE WHEN attempts + 1 < ?3 THEN ?6 ELSE NULL END,
                    last_error = ?4, updated_at = ?5
              WHERE id = ?1 AND claim_token = ?2",
            params![
                saturating_rowid(id),
                token,
                crate::enrichment::MAX_ATTEMPTS as i64,
                error,
                catalog::stamp(now),
                catalog::stamp(now + crate::enrichment::retry_backoff()),
            ],
        )?;
        Ok(())
    }

    /// Writes an asset's bytes into the cache and marks it present. The
    /// file is `<cache>/<asset_id>.img`; the row's `path` column points
    /// at it, so the view reads the local file and never the network.
    ///
    /// An image bigger than the cache's size cap is scaled down before
    /// it is written: the reader never needs the original, and the
    /// cache is a cache, not an archive.
    pub fn store_asset_bytes(
        &mut self,
        asset_id: &str,
        claim_token: &str,
        bytes: &[u8],
        cache_dir: &Path,
        now: DateTime<Utc>,
    ) -> Result<AssetRecord, StoreError> {
        let bytes = shrink_image(bytes);
        std::fs::create_dir_all(cache_dir).map_err(|error| {
            StoreError::Parse(format!(
                "the asset cache {} cannot be created: {error}",
                cache_dir.display()
            ))
        })?;
        let path = cache_dir.join(format!("{asset_id}.{ASSET_EXTENSION}"));
        std::fs::write(&path, &bytes).map_err(|error| {
            StoreError::Parse(format!(
                "the asset {} cannot be written: {error}",
                path.display()
            ))
        })?;
        let size = bytes.len() as u64;
        self.conn.execute(
            "UPDATE assets SET state = 'present', bytes = ?2, path = ?3, fetched_at = ?4
              WHERE asset_id = ?1",
            params![
                asset_id,
                size as i64,
                path.to_string_lossy().to_string(),
                catalog::stamp(now),
            ],
        )?;
        let url: String = self.conn.query_row(
            "SELECT url FROM assets WHERE asset_id = ?1",
            params![asset_id],
            |row| row.get(0),
        )?;
        // The queue row is stamped done under the claim's own token —
        // a fetch whose claim was re-taken mid-flight does not close
        // the new owner's job out from under it.
        self.conn.execute(
            "UPDATE asset_queue
             SET state = 'done', last_error = NULL, lease_until = NULL, not_before = NULL,
                 claim_token = NULL, updated_at = ?3
              WHERE asset_id = ?1 AND claim_token = ?2 AND state = 'claimed'",
            params![asset_id, claim_token, catalog::stamp(now)],
        )?;
        Ok(AssetRecord {
            asset_id: asset_id.to_string(),
            url,
            badge: false,
            state: "present".to_string(),
            path: Some(path),
            bytes: Some(size),
        })
    }

    /// Marks an asset as failed — it will not be fetched again until a
    /// later run re-enqueues it.
    pub fn mark_asset_failed(&mut self, asset_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE assets SET state = 'failed' WHERE asset_id = ?1",
            params![asset_id],
        )?;
        Ok(())
    }

    /// Evicts until the cache fits `cap_bytes`: the oldest-fetched
    /// assets first (by the `fetched_at` stamp), then the oldest
    /// digests (by `taken_at`) — one cap over both caches. A pinned
    /// asset (an open view is rendering it — plain asset ids cross the
    /// seam, no UI types) is never evicted, and neither is the newest
    /// asset: a cap smaller than one image empties everything but it
    /// rather than thrashing it. An evicted asset loses its file, goes
    /// back to `missing`, and leaves the queue's books — the next look
    /// re-enqueues and re-fetches it, which is what a cache is for. An
    /// evicted digest loses its row (and its full-text twin); the next
    /// open regenerates it on demand. Catalog rows, readmes, license
    /// texts and trees are never touched — those are the library, not
    /// the cache. A file that cannot be deleted (another handle holds
    /// it) is kept with its row intact and the walk moves on. The
    /// eviction counters accumulate in the app's state, for `stats`.
    pub fn prune_cache(
        &mut self,
        cache_dir: &Path,
        cap_bytes: u64,
        pinned: &[String],
    ) -> Result<PruneReport, StoreError> {
        let present: Vec<(String, u64)> = {
            let mut statement = self.conn.prepare(
                "SELECT asset_id, COALESCE(bytes, 0) FROM assets
              WHERE state = 'present'
              ORDER BY fetched_at ASC, asset_id ASC",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?.max(0) as u64,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let total: u64 = present.iter().map(|(_, bytes)| bytes).sum::<u64>()
            + crate::digests::stored_bytes(&self.conn);
        let mut evicted = 0u64;
        let mut evicted_bytes = 0u64;
        for (index, (asset_id, bytes)) in present.iter().enumerate() {
            if total - evicted_bytes <= cap_bytes {
                break;
            }
            if index + 1 >= present.len() {
                break;
            }
            if pinned.iter().any(|held| held == asset_id) {
                continue;
            }
            if !self.evict_asset(cache_dir, asset_id)? {
                continue;
            }
            evicted += 1;
            evicted_bytes += bytes;
        }
        let mut digests_evicted = 0u64;
        while total - evicted_bytes > cap_bytes {
            let Some(freed) = crate::digests::evict_oldest(&self.conn)? else {
                break;
            };
            digests_evicted += 1;
            evicted_bytes += freed;
        }
        self.bump_eviction_counters(evicted + digests_evicted, evicted_bytes)?;
        Ok(PruneReport {
            files: present.len() as u64,
            bytes: total,
            evicted,
            evicted_bytes,
            digests_evicted,
        })
    }

    /// Adds one prune pass to the eviction counters the stats line
    /// reports.
    fn bump_eviction_counters(&self, files: u64, bytes: u64) -> Result<(), StoreError> {
        if files == 0 {
            return Ok(());
        }
        self.bump_app_state_counter(crate::appstate::EVICTED_FILES_KEY, files)?;
        self.bump_app_state_counter(crate::appstate::EVICTED_BYTES_KEY, bytes)
    }

    /// The cache's per-kind byte counts: the cached images (files and
    /// bytes) and the stored digests (count and bytes) — the stats
    /// line's input.
    pub fn cache_breakdown(&self) -> Result<CacheBreakdown, StoreError> {
        let (asset_files, asset_bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM assets WHERE state = 'present'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (digest_count, digest_bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(spec_json) + LENGTH(body)
                                    + COALESCE(LENGTH(taken_commit), 0)), 0)
               FROM digests",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(CacheBreakdown {
            asset_files: asset_files.max(0) as u64,
            asset_bytes: asset_bytes.max(0) as u64,
            digest_count: digest_count.max(0) as u64,
            digest_bytes: digest_bytes.max(0) as u64,
        })
    }

    /// One asset out of the cache: the file deleted, the row back to
    /// `missing`, the queue's done row gone (a done row would refuse the
    /// re-enqueue). `false` when the file could not be deleted — the row
    /// stays as it is and the walk moves on.
    fn evict_asset(&mut self, cache_dir: &Path, asset_id: &str) -> Result<bool, StoreError> {
        let path = cache_dir.join(format!("{asset_id}.{ASSET_EXTENSION}"));
        if path.exists() && std::fs::remove_file(&path).is_err() {
            return Ok(false);
        }
        self.conn.execute(
            "UPDATE assets SET state = 'missing', bytes = NULL, path = NULL, fetched_at = NULL
          WHERE asset_id = ?1",
            params![asset_id],
        )?;
        self.conn.execute(
            "DELETE FROM asset_queue WHERE asset_id = ?1",
            params![asset_id],
        )?;
        Ok(true)
    }

    /// The first `lines` lines of a repository's stored README, and
    /// whether more follow. `None` when no README was fetched.
    pub fn readme_head(
        &self,
        repo_rowid: u64,
        lines: usize,
    ) -> Result<Option<ReadmeHead>, StoreError> {
        let original: Option<String> = self
            .conn
            .query_row(
                "SELECT original FROM readmes WHERE repo_rowid = ?1",
                params![saturating_rowid(repo_rowid)],
                |row| row.get(0),
            )
            .optional()?;
        let Some(original) = original else {
            return Ok(None);
        };
        let all: Vec<&str> = original.lines().collect();
        let head: Vec<&str> = all.iter().take(lines).copied().collect();
        Ok(Some(ReadmeHead {
            text: head.join("\n"),
            total_lines: all.len(),
            truncated: all.len() > head.len(),
        }))
    }
}

/// What the cache holds on disk: the file count and their total size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub files: u64,
    pub bytes: u64,
}

/// What one prune pass did: what the cache held (asset files on disk
/// plus digest bytes, against the one cap), what it evicted of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    pub files: u64,
    pub bytes: u64,
    pub evicted: u64,
    pub evicted_bytes: u64,
    pub digests_evicted: u64,
}

/// The cache's per-kind byte counts, as the stats line reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheBreakdown {
    pub asset_files: u64,
    pub asset_bytes: u64,
    pub digest_count: u64,
    pub digest_bytes: u64,
}

/// Walks the cache directory: the file count and their total size. A
/// directory that cannot be read is an empty cache, not an error — the
/// probe reports, and an unreadable cache will say so at the first
/// write.
pub fn cache_stats(dir: &Path) -> CacheStats {
    let mut stats = CacheStats::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return stats;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            stats.files += 1;
            stats.bytes += meta.len();
        }
    }
    stats
}

/// The bytes as the cache keeps them: an image bigger than the size
/// cap is scaled down in proportion, everything else is kept as it is.
///
/// Only the raster formats are scaled: an SVG is a vector (it draws at
/// any size), and an animated GIF would lose its animation.
pub fn shrink_image(bytes: &[u8]) -> Vec<u8> {
    let Ok(format) = image::guess_format(bytes) else {
        return bytes.to_vec();
    };
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return bytes.to_vec();
    }
    let Ok(decoded) = image::load_from_memory(bytes) else {
        return bytes.to_vec();
    };
    if decoded.width() <= IMAGE_MAX_WIDTH && decoded.height() <= IMAGE_MAX_HEIGHT {
        return bytes.to_vec();
    }
    let smaller = decoded.resize(
        IMAGE_MAX_WIDTH,
        IMAGE_MAX_HEIGHT,
        image::imageops::FilterType::Lanczos3,
    );
    let mut shrunk = Vec::new();
    let written = {
        let mut cursor = std::io::Cursor::new(&mut shrunk);
        smaller.write_to(&mut cursor, format)
    };
    if written.is_err() {
        return bytes.to_vec();
    }
    shrunk
}

/// One claimed asset job, read back by its queue id — the claim's last
/// step, shared by the global and the per-repository claim.
pub(crate) fn read_job(connection: &rusqlite::Connection, id: i64) -> Result<AssetJob, StoreError> {
    let mut statement = connection.prepare(
        "SELECT q.id, q.asset_id, a.url, q.claim_token
           FROM asset_queue q JOIN assets a ON a.asset_id = q.asset_id
          WHERE q.id = ?1",
    )?;
    let mut rows = statement.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Err(StoreError::Parse(format!(
            "asset queue row {id} vanished mid-claim"
        )));
    };
    Ok(AssetJob {
        id: row.get::<_, i64>(0)?.max(0) as u64,
        asset_id: row.get(1)?,
        url: row.get(2)?,
        claim_token: row.get(3)?,
    })
}

/// Maps one asset row into a record.
pub(crate) fn map_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRecord> {
    let path: Option<String> = row.get(4)?;
    Ok(AssetRecord {
        asset_id: row.get(0)?,
        url: row.get(1)?,
        badge: row.get::<_, i64>(2)? != 0,
        state: row.get(3)?,
        path: path.map(PathBuf::from),
        bytes: row.get::<_, Option<i64>>(5)?.map(|size| size.max(0) as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;
    use chrono::TimeZone;
    use std::fs;

    fn store(label: &str) -> (Scratch, Store) {
        let scratch = Scratch::new(label);
        let store = Store::open(&scratch.path()).expect("open");
        (scratch, store)
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
            .single()
            .expect("valid test instant")
    }

    fn seed(store: &mut Store, count: usize) -> Vec<u64> {
        let snapshots: Vec<yagna_core::model::RepoSnapshot> = (0..count)
            .map(|index| crate::testing::sample_snapshot("someone", &format!("repo-{index}")))
            .collect();
        store
            .upsert_snapshots(&snapshots, now())
            .expect("seed the catalog");
        let mut statement = store
            .conn
            .prepare("SELECT id FROM repos ORDER BY id")
            .expect("prepare");
        let mut rows = statement.query([]).expect("query");
        let mut ids = Vec::new();
        while let Some(row) = rows.next().expect("row") {
            ids.push(row.get::<_, i64>(0).expect("id") as u64);
        }
        ids
    }

    /// A README with a badge and two images — the shape the preview
    /// picks from.
    fn readme_with_images() -> String {
        "# Demo\n\n[![ci](https://img.shields.io/badge/ci-ok)](https://ci)\n\n\
         ![first](docs/first.png)\n\n![second](docs/second.png)\n"
            .to_string()
    }

    #[test]
    fn the_preview_is_the_first_non_badge_asset() {
        let (_scratch, mut store) = store("preview");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &readme_with_images(), now())
            .expect("store the readme");

        let asset = store
            .preview_asset(ids[0])
            .expect("preview")
            .expect("an asset");
        assert!(!asset.badge, "the badge is skipped");
        assert!(asset.url.ends_with("docs/first.png"), "{}", asset.url);
        assert_eq!(asset.state, "missing", "the bytes are not cached yet");
        assert!(asset.path.is_none());
    }

    #[test]
    fn a_repository_without_images_has_no_preview() {
        let (_scratch, mut store) = store("no-preview");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], "# Demo\n\nJust text.\n", now())
            .expect("store the readme");
        assert!(store.preview_asset(ids[0]).expect("preview").is_none());
    }

    #[test]
    fn a_repository_without_a_readme_has_no_preview() {
        let (_scratch, mut store) = store("no-readme");
        let ids = seed(&mut store, 1);
        assert!(store.preview_asset(ids[0]).expect("preview").is_none());
    }

    #[test]
    fn the_bytes_land_in_the_cache_and_the_row_points_at_the_file() {
        let (_scratch, mut store) = store("store-bytes");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &readme_with_images(), now())
            .expect("store the readme");
        let asset = store
            .preview_asset(ids[0])
            .expect("preview")
            .expect("an asset");
        let cache = std::env::temp_dir().join(format!("yagna-assets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);

        store
            .enqueue_asset(&asset.asset_id, now())
            .expect("enqueue");
        let job = store.claim_asset().expect("claim").expect("a job");
        assert_eq!(job.asset_id, asset.asset_id);
        assert_eq!(job.url, asset.url);

        let stored = store
            .store_asset_bytes(
                &asset.asset_id,
                &job.claim_token,
                b"fake-png-bytes",
                &cache,
                now(),
            )
            .expect("store");
        assert_eq!(stored.state, "present");
        assert_eq!(stored.bytes, Some(14));
        let path = stored.path.expect("a path");
        assert!(path.exists(), "the file is on disk");
        assert_eq!(
            fs::read(&path).expect("read"),
            b"fake-png-bytes",
            "the bytes are the ones that were written"
        );

        let preview = store
            .preview_asset(ids[0])
            .expect("preview")
            .expect("an asset");
        assert_eq!(preview.state, "present");
        assert_eq!(preview.path, Some(path));

        store
            .stamp_asset_done(job.id, &job.claim_token, now())
            .expect("stamp");
        assert!(
            store.claim_asset().expect("claim").is_none(),
            "the queue drained"
        );
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn enqueueing_an_asset_twice_queues_it_once() {
        let (_scratch, mut store) = store("enqueue-twice");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &readme_with_images(), now())
            .expect("store the readme");
        let asset = store
            .preview_asset(ids[0])
            .expect("preview")
            .expect("an asset");
        assert_eq!(
            store
                .enqueue_asset(&asset.asset_id, now())
                .expect("enqueue"),
            1
        );
        assert_eq!(
            store
                .enqueue_asset(&asset.asset_id, now())
                .expect("enqueue"),
            0,
            "the pending row is not duplicated"
        );
    }

    #[test]
    fn the_bulk_fill_skips_badges_and_cached_assets() {
        let (_scratch, mut store) = store("bulk");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &readme_with_images(), now())
            .expect("store the readme");
        let queued = store.enqueue_missing_assets(now()).expect("fill");
        assert_eq!(queued, 2, "the two images, not the badge");
        assert_eq!(
            store.enqueue_missing_assets(now()).expect("fill"),
            0,
            "the fill is idempotent"
        );
    }

    #[test]
    fn the_readme_head_is_the_first_lines_and_knows_whether_more_follow() {
        let (_scratch, mut store) = store("head");
        let ids = seed(&mut store, 1);
        let body = (1..=80)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        store
            .store_readme(ids[0], &body, now())
            .expect("store the readme");

        let head = store
            .readme_head(ids[0], 50)
            .expect("head")
            .expect("a head");
        assert_eq!(head.total_lines, 80);
        assert!(head.truncated);
        assert_eq!(head.text.lines().count(), 50);
        assert_eq!(head.text.lines().next(), Some("line 1"));

        let short = store
            .readme_head(ids[0], 100)
            .expect("head")
            .expect("a head");
        assert!(!short.truncated, "the whole README fits");
        assert_eq!(short.total_lines, 80);
    }

    #[test]
    fn a_big_image_is_scaled_down_and_a_small_one_is_kept() {
        let big = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            800,
            600,
            image::Rgb([12, 34, 56]),
        ));
        let mut encoded = Vec::new();
        big.write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .expect("encode");
        let shrunk = shrink_image(&encoded);
        let decoded = image::load_from_memory(&shrunk).expect("decode");
        assert!(
            decoded.width() <= IMAGE_MAX_WIDTH && decoded.height() <= IMAGE_MAX_HEIGHT,
            "the image fits the cap: {}x{}",
            decoded.width(),
            decoded.height()
        );
        assert!(
            shrunk.len() < encoded.len(),
            "and it is smaller: {} against {}",
            shrunk.len(),
            encoded.len()
        );

        let small = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            100,
            80,
            image::Rgb([12, 34, 56]),
        ));
        let mut encoded = Vec::new();
        small
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .expect("encode");
        assert_eq!(
            shrink_image(&encoded),
            encoded,
            "an image inside the cap is kept as it is"
        );
    }

    #[test]
    fn something_that_is_not_an_image_is_kept_as_it_is() {
        let bytes = b"not an image at all";
        assert_eq!(shrink_image(bytes), bytes);
    }

    #[test]
    fn a_repository_without_a_readme_has_no_head() {
        let (_scratch, mut store) = store("no-head");
        let ids = seed(&mut store, 1);
        assert!(store.readme_head(ids[0], 50).expect("head").is_none());
    }

    /// Three present assets cached at distinct instants, a cache
    /// directory of real files — the shape the prune tests walk.
    fn store_three_assets(label: &str, cache: &Path) -> (Scratch, Store, Vec<String>) {
        let (scratch, mut store) = store(label);
        let ids = seed(&mut store, 3);
        for &rowid in &ids {
            store
                .store_readme(rowid, &readme_with_images(), now())
                .expect("store the readme");
        }
        let mut asset_ids = Vec::new();
        for (index, &rowid) in ids.iter().enumerate() {
            let asset = store
                .preview_asset(rowid)
                .expect("preview")
                .expect("an asset");
            let at = Utc
                .with_ymd_and_hms(2026, 10, 1 + index as u32, 12, 0, 0)
                .single()
                .expect("valid instant");
            store
                .store_asset_bytes(&asset.asset_id, "", b"0123456789", cache, at)
                .expect("store bytes");
            asset_ids.push(asset.asset_id);
        }
        (scratch, store, asset_ids)
    }

    fn prune_cache_dir(label: &str) -> PathBuf {
        let cache =
            std::env::temp_dir().join(format!("yagna-prune-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);
        fs::create_dir_all(&cache).expect("cache directory");
        cache
    }

    /// The oldest-fetched assets go first, until the cache fits the
    /// cap; the survivors keep their files and their rows.
    #[test]
    fn the_prune_evicts_oldest_first_until_the_cap_fits() {
        let cache = prune_cache_dir("prune-oldest");
        let (_scratch, mut store, asset_ids) = store_three_assets("prune-oldest", &cache);
        // Each asset is 10 bytes; the cap fits two.
        let report = store.prune_cache(&cache, 20, &[]).expect("prune");
        assert_eq!(report.files, 3);
        assert_eq!(report.bytes, 30);
        assert_eq!(report.evicted, 1);
        assert_eq!(report.evicted_bytes, 10);
        assert!(
            !cache.join(format!("{}.img", asset_ids[0])).exists(),
            "the oldest file is gone"
        );
        assert!(cache.join(format!("{}.img", asset_ids[1])).exists());
        assert!(cache.join(format!("{}.img", asset_ids[2])).exists());
        let first = store.asset(&asset_ids[0]).expect("row").expect("the row");
        assert_eq!(first.state, "missing", "the evicted row is back to missing");
        assert_eq!(first.bytes, None);
        assert_eq!(first.path, None);
        let second = store.asset(&asset_ids[1]).expect("row").expect("the row");
        assert_eq!(second.state, "present");
        let _ = fs::remove_dir_all(&cache);
    }

    /// A cache under the cap is untouched.
    #[test]
    fn the_prune_leaves_a_cache_that_fits_alone() {
        let cache = prune_cache_dir("prune-fits");
        let (_scratch, mut store, asset_ids) = store_three_assets("prune-fits", &cache);
        let report = store.prune_cache(&cache, 30, &[]).expect("prune");
        assert_eq!(report.evicted, 0);
        assert!(cache.join(format!("{}.img", asset_ids[0])).exists());
        let _ = fs::remove_dir_all(&cache);
    }

    /// The newest asset is never evicted, even when it alone exceeds
    /// the cap: one image always survives rather than thrashing.
    #[test]
    fn the_newest_asset_survives_any_cap() {
        let cache = prune_cache_dir("prune-newest");
        let (_scratch, mut store, asset_ids) = store_three_assets("prune-newest", &cache);
        let report = store.prune_cache(&cache, 5, &[]).expect("prune");
        assert_eq!(report.evicted, 2, "everything but the newest goes");
        assert!(cache.join(format!("{}.img", asset_ids[2])).exists());
        assert_eq!(
            store.prune_cache(&cache, 5, &[]).expect("prune").evicted,
            0,
            "the last one stands"
        );
        let _ = fs::remove_dir_all(&cache);
    }

    /// An evicted asset can be re-enqueued and re-stored — the queue's
    /// done row went with the file.
    #[test]
    fn an_evicted_asset_refetches_cleanly() {
        let cache = prune_cache_dir("prune-refetch");
        let (_scratch, mut store, asset_ids) = store_three_assets("prune-refetch", &cache);
        store.prune_cache(&cache, 20, &[]).expect("prune");
        let queued = store
            .enqueue_asset(&asset_ids[0], now())
            .expect("re-enqueue");
        assert_eq!(queued, 1, "the done row did not block the re-enqueue");
        let stored = store
            .store_asset_bytes(&asset_ids[0], "", b"fresh-bytes", &cache, now())
            .expect("re-store");
        assert_eq!(stored.state, "present");
        assert!(stored.path.expect("a path").exists());
        let _ = fs::remove_dir_all(&cache);
    }

    /// The disk walk counts the files and their bytes.
    #[test]
    fn the_cache_stats_walk_the_directory() {
        let cache = prune_cache_dir("stats");
        fs::write(cache.join("a.img"), b"12345").expect("write");
        fs::write(cache.join("b.img"), b"123").expect("write");
        let stats = cache_stats(&cache);
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 8);
        assert_eq!(cache_stats(&cache.join("absent")), CacheStats::default());
        let _ = fs::remove_dir_all(&cache);
    }

    /// A README with a single image — the one-job queue shape.
    fn one_image_readme() -> String {
        "# Demo\n\n![one](docs/one.png)\n".to_string()
    }

    /// The asset claim is a lease: a live lease is not re-claimed, and
    /// a lapsed one — the kill mid-fetch — is picked up again.
    #[test]
    fn an_asset_claim_holds_a_lease_and_a_killed_run_resumes() {
        let (_scratch, mut store) = store("asset-lease");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &readme_with_images(), now())
            .expect("readme");
        store.enqueue_missing_assets(now()).expect("fill");

        let job = store.claim_asset().expect("claim").expect("a job");
        let (state, lease): (String, Option<String>) = store
            .conn
            .query_row(
                "SELECT state, lease_until FROM asset_queue WHERE id = ?1",
                params![job.id as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(state, "claimed");
        assert!(lease.is_some(), "the claim carries a lease");

        let other = store.claim_asset().expect("claim").expect("an item");
        assert_ne!(other.id, job.id, "a live lease does not block the rest");

        store
            .conn
            .execute(
                "UPDATE asset_queue SET lease_until = '2026-01-01T00:00:00Z' WHERE id = ?1",
                params![job.id as i64],
            )
            .expect("lapse the lease");
        let again = store.claim_asset().expect("claim").expect("again");
        assert_eq!(again.id, job.id, "the abandoned fetch is picked up again");
    }

    /// A released claim waits out its backoff instead of hot-looping.
    #[test]
    fn a_released_asset_claim_waits_out_its_backoff() {
        let (_scratch, mut store) = store("asset-release");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &one_image_readme(), now())
            .expect("readme");
        store.enqueue_missing_assets(now()).expect("fill");
        let job = store.claim_asset().expect("claim").expect("a job");
        store
            .release_asset_claim(job.id, &job.claim_token, Utc::now())
            .expect("release");
        assert!(
            store.claim_asset().expect("claim").is_none(),
            "the released row waits"
        );
        assert!(
            store.asset_backoff_until().expect("backoff").is_some(),
            "the wait is reported"
        );
        store
            .conn
            .execute(
                "UPDATE asset_queue SET not_before = NULL WHERE id = ?1",
                params![job.id as i64],
            )
            .expect("time passes, the wait is over");
        assert!(
            store.claim_asset().expect("claim").is_some(),
            "a past stamp does not block"
        );
    }

    /// A pinned asset — an open view is rendering it — survives a prune
    /// that would have evicted it; the next-oldest goes instead.
    #[test]
    fn a_pinned_asset_survives_the_prune() {
        let cache = prune_cache_dir("prune-pinned");
        let (_scratch, mut store, asset_ids) = store_three_assets("prune-pinned", &cache);
        let report = store
            .prune_cache(&cache, 20, std::slice::from_ref(&asset_ids[0]))
            .expect("prune");
        assert_eq!(report.evicted, 1);
        assert!(
            cache.join(format!("{}.img", asset_ids[0])).exists(),
            "the pinned file survives"
        );
        assert!(
            !cache.join(format!("{}.img", asset_ids[1])).exists(),
            "the next-oldest was evicted instead"
        );
        let _ = fs::remove_dir_all(&cache);
    }

    /// The digest cache shares the asset cache's cap: when the assets
    /// alone overfit it, the oldest digests go first — the row and its
    /// full-text twin — and the library stays untouched.
    #[test]
    fn the_prune_evicts_digests_when_the_assets_do_not_fit_the_cap() {
        use yagna_core::digest::DigestInput;
        use yagna_core::model::{Forge, RepoRef};
        let cache = prune_cache_dir("prune-digest");
        let (_scratch, mut store) = store("prune-digest");
        let ids = seed(&mut store, 1);
        store
            .store_readme(ids[0], &one_image_readme(), now())
            .expect("readme");
        let asset = store
            .preview_asset(ids[0])
            .expect("preview")
            .expect("an asset");
        store
            .store_asset_bytes(&asset.asset_id, "", b"0123456789", &cache, now())
            .expect("store bytes");
        let repo = RepoRef {
            forge: Forge::GitHub,
            owner: "someone".to_string(),
            name: "repo-0".to_string(),
        };
        let input = DigestInput {
            repo,
            files: vec![yagna_core::digest::DigestFile {
                path: "README.md".to_string(),
                bytes: 400,
                text: Some("a digest body that takes real room\n".to_string()),
            }],
            truncated_by: None,
            taken_commit: Some("abc123".to_string()),
        };
        store
            .store_digest(
                ids[0],
                &yagna_core::digest::standard_spec(&input.repo),
                &input,
                now(),
            )
            .expect("digest");

        let report = store.prune_cache(&cache, 15, &[]).expect("prune");
        assert_eq!(report.evicted, 0, "the one asset is never thrashed");
        assert_eq!(
            report.digests_evicted, 1,
            "the digest paid for the cap instead"
        );
        assert!(
            store.digest_body(ids[0]).expect("read").is_none(),
            "the digest row is gone"
        );
        let hits: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_digest WHERE fts_digest MATCH 'digest'",
                [],
                |row| row.get(0),
            )
            .expect("fts count");
        assert_eq!(hits, 0, "the full-text twin went with it");

        let breakdown = store.cache_breakdown().expect("breakdown");
        assert_eq!(breakdown.asset_files, 1);
        assert_eq!(breakdown.asset_bytes, 10);
        assert_eq!(breakdown.digest_count, 0);
        assert_eq!(
            store
                .app_state_counter(crate::appstate::EVICTED_FILES_KEY)
                .expect("counter"),
            1,
            "the eviction is on the books"
        );
        let _ = fs::remove_dir_all(&cache);
    }
}
