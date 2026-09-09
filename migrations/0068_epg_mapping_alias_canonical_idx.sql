-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channel_aliases_normalized_canonical_idx ON channel_aliases (normalize_channel_name(canonical_name));
