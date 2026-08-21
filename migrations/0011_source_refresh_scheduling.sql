-- Add configurable refresh interval and last refreshed timestamp to sources.
-- refresh_interval_seconds controls how often the scheduler enqueues a refresh job.
-- A value of 0 disables automatic refresh for that source.
-- last_refreshed_at records when a refresh job last completed for the source.

ALTER TABLE provider_accounts
    ADD COLUMN IF NOT EXISTS refresh_interval_seconds integer NOT NULL DEFAULT 0
    CHECK (refresh_interval_seconds >= 0),
    ADD COLUMN IF NOT EXISTS last_refreshed_at timestamptz;

ALTER TABLE epg_sources
    ADD COLUMN IF NOT EXISTS refresh_interval_seconds integer NOT NULL DEFAULT 0
    CHECK (refresh_interval_seconds >= 0),
    ADD COLUMN IF NOT EXISTS last_refreshed_at timestamptz;
