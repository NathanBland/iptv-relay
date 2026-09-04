ALTER TABLE provider_reconciliation_runs
    ADD COLUMN IF NOT EXISTS parent_job_id uuid REFERENCES jobs(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS provider_reconciliation_runs_parent_job_idx
    ON provider_reconciliation_runs (parent_job_id)
    WHERE parent_job_id IS NOT NULL;
