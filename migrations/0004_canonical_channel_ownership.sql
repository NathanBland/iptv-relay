ALTER TABLE channels
    ADD COLUMN IF NOT EXISTS provider_account_id uuid REFERENCES provider_accounts(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS canonical_key text;

CREATE INDEX IF NOT EXISTS channels_provider_account_idx
    ON channels(provider_account_id)
    WHERE provider_account_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS channels_provider_canonical_key_idx
    ON channels(provider_account_id, canonical_key)
    WHERE provider_account_id IS NOT NULL AND canonical_key IS NOT NULL;

CREATE SEQUENCE IF NOT EXISTS canonical_channel_number_seq
    AS integer START 1000000;

CREATE INDEX IF NOT EXISTS channel_streams_provider_stream_idx
    ON channel_streams(provider_stream_id);
