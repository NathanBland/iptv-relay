-- Keep Xtream guide snapshots separate from Xtream live catalog snapshots.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'source_snapshots_kind_check'
          AND conrelid = 'source_snapshots'::regclass
    ) THEN
        ALTER TABLE source_snapshots DROP CONSTRAINT source_snapshots_kind_check;
    END IF;
END $$;

ALTER TABLE source_snapshots
    ADD CONSTRAINT source_snapshots_kind_check
    CHECK (kind IN ('m3u', 'xtream', 'xtream-epg', 'xmltv', 'network-tuner'));

ALTER TABLE epg_channels
    ADD COLUMN IF NOT EXISTS provider_account_id uuid
    REFERENCES provider_accounts(id) ON DELETE CASCADE;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'epg_channels_exactly_one_owner_check'
          AND conrelid = 'epg_channels'::regclass
    ) THEN
        ALTER TABLE epg_channels
            ADD CONSTRAINT epg_channels_exactly_one_owner_check
            CHECK ((epg_source_id IS NULL) <> (provider_account_id IS NULL));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS epg_channels_provider_xmltv_idx
    ON epg_channels(provider_account_id, xmltv_id)
    WHERE provider_account_id IS NOT NULL;
