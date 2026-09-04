-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS user_channel_access_channel_idx
    ON user_channel_access(channel_id);
