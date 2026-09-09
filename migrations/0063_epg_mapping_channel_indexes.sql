-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channels_lower_canonical_key_idx ON channels (lower(canonical_key), id) WHERE managed_by = 'automatic' AND canonical_key IS NOT NULL;
