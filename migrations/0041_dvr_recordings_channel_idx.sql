-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS dvr_recordings_channel_idx
    ON dvr_recordings(channel_id);
