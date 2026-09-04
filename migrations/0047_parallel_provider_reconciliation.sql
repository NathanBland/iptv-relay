CREATE TABLE IF NOT EXISTS provider_reconciliation_runs (
    id uuid PRIMARY KEY,
    provider_account_id uuid NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    source_snapshot_id uuid NOT NULL UNIQUE REFERENCES source_snapshots(id) ON DELETE CASCADE,
    status text NOT NULL CHECK (status IN ('queued', 'processing', 'failed', 'published', 'cancelled')),
    partition_count integer NOT NULL CHECK (partition_count > 0),
    total_keys bigint NOT NULL DEFAULT 0 CHECK (total_keys >= 0),
    completed_keys bigint NOT NULL DEFAULT 0 CHECK (completed_keys >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    published_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS provider_reconciliation_runs_account_status_idx
    ON provider_reconciliation_runs (provider_account_id, status, created_at DESC);

CREATE TABLE IF NOT EXISTS provider_reconciliation_partitions (
    run_id uuid NOT NULL REFERENCES provider_reconciliation_runs(id) ON DELETE CASCADE,
    partition_number integer NOT NULL CHECK (partition_number >= 0),
    status text NOT NULL CHECK (status IN ('queued', 'processing', 'succeeded', 'failed', 'cancelled')),
    key_count bigint NOT NULL DEFAULT 0 CHECK (key_count >= 0),
    completed_keys bigint NOT NULL DEFAULT 0 CHECK (completed_keys >= 0),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    worker_id text,
    completed_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, partition_number)
);

CREATE INDEX IF NOT EXISTS provider_reconciliation_partitions_status_idx
    ON provider_reconciliation_partitions (run_id, status, partition_number);

CREATE TABLE IF NOT EXISTS provider_reconciliation_candidates (
    run_id uuid NOT NULL REFERENCES provider_reconciliation_runs(id) ON DELETE CASCADE,
    canonical_key text NOT NULL,
    name text NOT NULL,
    group_name text,
    logo_url text,
    preferred_number text,
    PRIMARY KEY (run_id, canonical_key)
);

CREATE TABLE IF NOT EXISTS provider_reconciliation_candidate_streams (
    run_id uuid NOT NULL,
    canonical_key text NOT NULL,
    provider_stream_id uuid NOT NULL REFERENCES provider_streams(id) ON DELETE CASCADE,
    PRIMARY KEY (run_id, canonical_key, provider_stream_id),
    FOREIGN KEY (run_id, canonical_key)
        REFERENCES provider_reconciliation_candidates(run_id, canonical_key)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS provider_reconciliation_candidate_streams_stream_idx
    ON provider_reconciliation_candidate_streams (provider_stream_id);
