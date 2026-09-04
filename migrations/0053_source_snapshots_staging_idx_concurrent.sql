-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS source_snapshots_staging_idx ON source_snapshots(provider_account_id, kind, fetched_at) WHERE status = 'staging';
