-- Performance indexes for large catalogs (100K+ channels, 1M+ provider streams).
-- Addresses slow channel listing, health probe selection, and provider stream updates.

-- Enable pg_trgm for fast ILIKE/LIKE search on channel names and groups.
-- Without this, lower(name) LIKE '%query%' scans every row in channels.
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;

-- Trigram indexes for channel search by name and group_name.
-- Used by list_channels, list_unmapped_channels, and EPG mapping queries.
CREATE INDEX IF NOT EXISTS channels_name_trgm_idx
    ON channels USING gin (name gin_trgm_ops);

CREATE INDEX IF NOT EXISTS channels_group_name_trgm_idx
    ON channels USING gin (group_name gin_trgm_ops)
    WHERE group_name IS NOT NULL;

-- Partial index for enabled channels only.
-- The channel list page filters by enabled in most views.
CREATE INDEX IF NOT EXISTS channels_enabled_id_idx
    ON channels(id)
    WHERE enabled = true;

-- Composite index for the health probe query.
-- The query filters on supported, health_status, and provider_account_id,
-- then joins channel_streams and channels(enabled).
-- This index lets the planner start from eligible provider streams
-- without scanning the full 1M+ row table.
CREATE INDEX IF NOT EXISTS provider_streams_health_probe_idx
    ON provider_streams(provider_account_id, health_status)
    WHERE supported = true
      AND health_status IN ('unknown', 'dead', 'checking');

-- Index channel_streams from the channel side for the health probe join.
-- The health probe joins channel_streams to channels WHERE c.enabled.
-- This index lets the planner use enabled channels as the driving table.
CREATE INDEX IF NOT EXISTS channel_streams_channel_id_enabled_idx
    ON channel_streams(channel_id)
    INCLUDE (provider_stream_id);

-- Partial index on channels(enabled) for the health probe join.
-- The planner can use this to filter enabled channels before joining.
CREATE INDEX IF NOT EXISTS channels_enabled_channel_number_idx
    ON channels(channel_number)
    WHERE enabled = true;

-- Non-partial index for the stream health page filter.
-- The UI filters by any health_status value, so the partial index
-- in migration 0013 (unknown/dead only) does not cover all filters.
CREATE INDEX IF NOT EXISTS provider_streams_health_status_full_idx
    ON provider_streams(health_status, group_name)
    WHERE supported = true;

-- Update planner statistics after adding new indexes.
ANALYZE channels;
ANALYZE channel_streams;
ANALYZE provider_streams;
