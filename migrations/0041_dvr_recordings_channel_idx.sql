-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS dvr_recordings_channel_idx
    ON recordings(channel_id);
