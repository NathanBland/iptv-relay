-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS source_snapshots_provider_account_idx
    ON source_snapshots(provider_account_id);
