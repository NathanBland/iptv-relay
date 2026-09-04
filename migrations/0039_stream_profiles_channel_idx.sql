-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS stream_profiles_channel_idx
    ON stream_profiles(channel_id);
