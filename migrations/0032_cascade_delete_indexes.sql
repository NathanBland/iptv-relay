-- Foreign key cascade indexes for fast source deletion.
-- Without these, deleting a provider with millions of streams causes
-- sequential scans on every child table during ON DELETE CASCADE.

-- source_snapshots: cascaded from provider_accounts(id)
-- Without this index, PostgreSQL scans all snapshots when deleting a provider.
CREATE INDEX IF NOT EXISTS source_snapshots_provider_account_idx
    ON source_snapshots(provider_account_id);

-- programmes: cascaded from source_snapshots(id)
-- The programmes table can have millions of rows. Without this index,
-- each snapshot deletion scans the full programmes table.
CREATE INDEX IF NOT EXISTS programmes_source_snapshot_idx
    ON programmes(source_snapshot_id);

-- epg_channels: cascaded from source_snapshots(id)
-- Without this index, each snapshot deletion scans all EPG channels.
CREATE INDEX IF NOT EXISTS epg_channels_source_snapshot_idx
    ON epg_channels(source_snapshot_id);

-- channel_epg_mappings: cascaded from channels(id) and epg_channels(id)
-- The channel_epg_mappings table has one row per mapped channel.
-- Without indexes on both FK columns, cascade deletes scan the full table.
CREATE INDEX IF NOT EXISTS channel_epg_mappings_epg_channel_idx
    ON channel_epg_mappings(epg_channel_id);

-- Expression index for jobs.payload->>'sourceId' used by source deletion
-- and sync status queries. Without this, every source operation scans
-- the full jobs table.
CREATE INDEX IF NOT EXISTS jobs_payload_source_id_idx
    ON jobs ((payload->>'sourceId'))
    WHERE payload ? 'sourceId';

-- audit_events: filtered by resource_id during source deletion cleanup
CREATE INDEX IF NOT EXISTS audit_events_resource_id_idx
    ON audit_events(resource_id);

-- generated_programmes: cascaded from channels(id)
CREATE INDEX IF NOT EXISTS generated_programmes_channel_idx
    ON generated_programmes(channel_id);

-- stream_profiles: cascaded from channels(id)
CREATE INDEX IF NOT EXISTS stream_profiles_channel_idx
    ON stream_profiles(channel_id);

-- user_channel_access: cascaded from channels(id)
CREATE INDEX IF NOT EXISTS user_channel_access_channel_idx
    ON user_channel_access(channel_id);

-- dvr_recordings: cascaded from channels(id)
CREATE INDEX IF NOT EXISTS dvr_recordings_channel_idx
    ON dvr_recordings(channel_id);

-- recording_rules: cascaded from channels(id)
CREATE INDEX IF NOT EXISTS recording_rules_channel_idx
    ON recording_rules(channel_id);

-- Update planner statistics after adding new indexes.
ANALYZE source_snapshots;
ANALYZE programmes;
ANALYZE epg_channels;
ANALYZE channel_epg_mappings;
ANALYZE jobs;
ANALYZE audit_events;
