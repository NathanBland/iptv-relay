-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS programmes_source_snapshot_idx
    ON programmes(source_snapshot_id);
