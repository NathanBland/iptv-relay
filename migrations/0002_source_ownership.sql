ALTER TABLE provider_accounts DROP CONSTRAINT provider_accounts_source_type_check;
ALTER TABLE provider_accounts ADD CONSTRAINT provider_accounts_source_type_check
    CHECK (source_type IN ('m3u', 'xtream', 'network-tuner'));

ALTER TABLE source_snapshots ALTER COLUMN provider_account_id DROP NOT NULL;
ALTER TABLE source_snapshots
    ADD COLUMN epg_source_id uuid REFERENCES epg_sources(id) ON DELETE CASCADE;
ALTER TABLE source_snapshots DROP CONSTRAINT source_snapshots_kind_check;
ALTER TABLE source_snapshots ADD CONSTRAINT source_snapshots_kind_check
    CHECK (kind IN ('m3u', 'xtream', 'xmltv', 'network-tuner'));
ALTER TABLE source_snapshots ADD CONSTRAINT source_snapshots_exactly_one_owner_check
    CHECK (
        (kind = 'xmltv' AND epg_source_id IS NOT NULL AND provider_account_id IS NULL)
        OR
        (kind IN ('m3u', 'xtream', 'network-tuner')
            AND provider_account_id IS NOT NULL AND epg_source_id IS NULL)
    );

CREATE UNIQUE INDEX source_snapshots_epg_checksum_idx
    ON source_snapshots(epg_source_id, kind, checksum_sha256)
    WHERE epg_source_id IS NOT NULL;
CREATE UNIQUE INDEX one_active_snapshot_per_epg_source
    ON source_snapshots(epg_source_id, kind)
    WHERE status = 'active' AND epg_source_id IS NOT NULL;
