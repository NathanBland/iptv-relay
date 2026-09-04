-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS generated_programmes_channel_idx
    ON generated_programmes(channel_id);
