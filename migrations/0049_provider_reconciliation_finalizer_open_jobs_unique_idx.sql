-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS jobs_open_provider_reconciliation_finalizer_unique_idx ON jobs ((payload->>'runId')) WHERE kind = 'finalize-provider-reconciliation' AND status IN ('queued', 'running');
