-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channel_aliases_normalized_alias_idx ON channel_aliases (normalize_channel_name(alias));
