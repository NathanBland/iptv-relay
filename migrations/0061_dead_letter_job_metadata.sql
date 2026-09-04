ALTER TABLE dead_letter_jobs ADD COLUMN IF NOT EXISTS failure_stage text;

ALTER TABLE dead_letter_jobs ADD COLUMN IF NOT EXISTS correlation_id uuid;

ALTER TABLE dead_letter_jobs ADD COLUMN IF NOT EXISTS run_id uuid;
