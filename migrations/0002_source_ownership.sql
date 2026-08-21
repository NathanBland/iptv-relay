-- Replace the source_type check constraint to include 'network-tuner'.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'provider_accounts_source_type_check'
          AND conrelid = 'provider_accounts'::regclass
    ) THEN
        ALTER TABLE provider_accounts DROP CONSTRAINT provider_accounts_source_type_check;
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'provider_accounts_source_type_check'
          AND conrelid = 'provider_accounts'::regclass
    ) THEN
        ALTER TABLE provider_accounts ADD CONSTRAINT provider_accounts_source_type_check
            CHECK (source_type IN ('m3u', 'xtream', 'network-tuner'));
    END IF;
END $$;

-- Allow source_snapshots to reference epg_sources instead of provider_accounts.
ALTER TABLE source_snapshots ALTER COLUMN provider_account_id DROP NOT NULL;

ALTER TABLE source_snapshots
    ADD COLUMN IF NOT EXISTS epg_source_id uuid REFERENCES epg_sources(id) ON DELETE CASCADE;

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

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'source_snapshots_kind_check'
          AND conrelid = 'source_snapshots'::regclass
    ) THEN
        ALTER TABLE source_snapshots ADD CONSTRAINT source_snapshots_kind_check
            CHECK (kind IN ('m3u', 'xtream', 'xmltv', 'network-tuner'));
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'source_snapshots_exactly_one_owner_check'
          AND conrelid = 'source_snapshots'::regclass
    ) THEN
        ALTER TABLE source_snapshots ADD CONSTRAINT source_snapshots_exactly_one_owner_check
            CHECK (
                (kind = 'xmltv' AND epg_source_id IS NOT NULL AND provider_account_id IS NULL)
                OR
                (kind IN ('m3u', 'xtream', 'network-tuner')
                    AND provider_account_id IS NOT NULL AND epg_source_id IS NULL)
            );
    END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS source_snapshots_epg_checksum_idx
    ON source_snapshots(epg_source_id, kind, checksum_sha256)
    WHERE epg_source_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS one_active_snapshot_per_epg_source
    ON source_snapshots(epg_source_id, kind)
    WHERE status = 'active' AND epg_source_id IS NOT NULL;
