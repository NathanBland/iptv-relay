ALTER TABLE provider_accounts
    ADD COLUMN IF NOT EXISTS cb_consecutive_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cb_opened_at timestamptz;

ALTER TABLE epg_sources
    ADD COLUMN IF NOT EXISTS cb_consecutive_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cb_opened_at timestamptz;
