-- Migration 0024 added the 'xtream-epg' kind to source_snapshots but did not
-- update the exactly-one-owner check. Short EPG snapshots are owned by the
-- provider account, so the owner check must include 'xtream-epg' alongside
-- 'm3u', 'xtream', and 'network-tuner'.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'source_snapshots_exactly_one_owner_check'
          AND conrelid = 'source_snapshots'::regclass
    ) THEN
        ALTER TABLE source_snapshots DROP CONSTRAINT source_snapshots_exactly_one_owner_check;
    END IF;
END $$;

ALTER TABLE source_snapshots
    ADD CONSTRAINT source_snapshots_exactly_one_owner_check
    CHECK (
        (kind = 'xmltv' AND epg_source_id IS NOT NULL AND provider_account_id IS NULL)
        OR
        (kind IN ('m3u', 'xtream', 'xtream-epg', 'network-tuner')
            AND provider_account_id IS NOT NULL AND epg_source_id IS NULL)
    );
