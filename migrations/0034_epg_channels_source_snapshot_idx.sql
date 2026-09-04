-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS epg_channels_source_snapshot_idx
    ON epg_channels(source_snapshot_id);
