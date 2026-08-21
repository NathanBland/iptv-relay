-- Add index for dead-letter queue queries
CREATE INDEX IF NOT EXISTS jobs_failed_idx ON jobs(updated_at DESC)
    WHERE status = 'failed';
