CREATE TABLE IF NOT EXISTS job_outbox (
    id uuid PRIMARY KEY,
    parent_job_id uuid NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    kind text NOT NULL,
    payload jsonb NOT NULL,
    priority integer NOT NULL DEFAULT 0,
    dedup_key text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    dispatched_at timestamptz
);

CREATE INDEX IF NOT EXISTS job_outbox_pending_idx ON job_outbox(created_at) WHERE dispatched_at IS NULL;
