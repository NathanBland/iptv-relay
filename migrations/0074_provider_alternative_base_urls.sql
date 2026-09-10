ALTER TABLE provider_accounts
    ADD COLUMN IF NOT EXISTS alternative_base_urls jsonb NOT NULL DEFAULT '[]'::jsonb;
