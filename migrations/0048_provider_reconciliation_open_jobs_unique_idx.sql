-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS jobs_open_provider_reconciliation_unique_idx ON jobs ((payload->>'runId'), (payload->>'partitionNumber')) WHERE kind = 'reconcile-provider-partition' AND status IN ('queued', 'running');
