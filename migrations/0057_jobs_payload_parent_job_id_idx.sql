-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS jobs_payload_parent_job_id_idx ON jobs ((payload->>'parentJobId')) WHERE kind IN ('reconcile-provider-partition', 'finalize-provider-reconciliation') AND status IN ('queued', 'running');
