-- Index the shared revisions table for reconciliation rollback lookups.
-- Reconciliation snapshots reuse the existing revisions table with
-- resource_type = 'reconciliation' and resource_id set to the provider
-- account id. No new table is added so migration history stays sequential.
CREATE INDEX IF NOT EXISTS revisions_reconciliation_resource_idx
    ON revisions (resource_id, revision DESC)
    WHERE resource_type = 'reconciliation';
