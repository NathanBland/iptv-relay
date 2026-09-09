ALTER TABLE source_snapshots
    ADD COLUMN IF NOT EXISTS staged_at timestamptz;
