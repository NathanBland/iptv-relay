-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS stream_profiles_channel_idx
    ON channel_stream_profiles(stream_profile_id);
