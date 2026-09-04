-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS recording_rules_channel_idx
    ON recording_rules(channel_id);
