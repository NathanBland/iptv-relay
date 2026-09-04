CREATE TABLE IF NOT EXISTS dead_letter_jobs (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    original_job_id uuid NOT NULL,
    kind            text NOT NULL,
    payload         jsonb NOT NULL DEFAULT '{}',
    error_category  text NOT NULL DEFAULT 'permanent',
    error_message   text NOT NULL DEFAULT '',
    attempt_count   integer NOT NULL DEFAULT 0,
    max_attempts    integer NOT NULL DEFAULT 0,
    first_failed_at timestamptz NOT NULL DEFAULT now(),
    last_failed_at  timestamptz NOT NULL DEFAULT now(),
    resolved_at     timestamptz,
    resolution      text,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_dead_letter_jobs_kind
    ON dead_letter_jobs (kind);

CREATE INDEX IF NOT EXISTS idx_dead_letter_jobs_error_category
    ON dead_letter_jobs (error_category);

CREATE INDEX IF NOT EXISTS idx_dead_letter_jobs_created_at
    ON dead_letter_jobs (created_at);

CREATE INDEX IF NOT EXISTS idx_dead_letter_jobs_resolved_at
    ON dead_letter_jobs (resolved_at)
    WHERE resolved_at IS NULL;
