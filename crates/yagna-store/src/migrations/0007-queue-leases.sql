-- Schema version 7: queue leases and backoff stamps.
-- Every long job resumes: a claim is a state ('claimed') with a lease
-- deadline ('lease_until') the owning runner heartbeats forward; a row
-- whose lease lapsed is claimable again, so a killed run's in-flight job
-- is picked up by the next one. 'not_before' is a backoff stamp: a row
-- stamped with a future instant is invisible to claims until it passes —
-- the queue's pause on rate limits and failed attempts, visible to the
-- runner as "waiting until".
-- Dates are UTC RFC 3339 strings; lexicographic order is chronological.

ALTER TABLE enrichment_queue ADD COLUMN lease_until TEXT;
ALTER TABLE enrichment_queue ADD COLUMN not_before TEXT;

ALTER TABLE asset_queue ADD COLUMN lease_until TEXT;
ALTER TABLE asset_queue ADD COLUMN not_before TEXT;
