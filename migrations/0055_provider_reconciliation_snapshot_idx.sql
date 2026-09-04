-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS provider_reconciliation_runs_snapshot_idx ON provider_reconciliation_runs(source_snapshot_id, created_at DESC, id DESC);
