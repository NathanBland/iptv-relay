-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channels_normalized_name_idx ON channels (normalize_channel_name(name), id) WHERE managed_by = 'automatic' AND canonical_key IS NOT NULL;
