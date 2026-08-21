-- 3NF and performance migration.
--
-- Drops unused indexes that waste space and write bandwidth.
-- Adds partial indexes for count queries that currently do full seq scans.
-- Adds a covering index for the groups aggregation query.
-- Runs ANALYZE to update planner statistics after structural changes.

-- Drop unused indexes (0 scans per pg_stat_user_indexes).
DROP INDEX IF EXISTS provider_streams_group_idx;
DROP INDEX IF EXISTS channels_group_name_idx;

-- Partial index on channels(enabled) for index-only count scans.
-- The count query `SELECT count(*) FROM channels WHERE enabled` currently
-- does a parallel seq scan over 1.16M rows. This partial index covers only
-- enabled rows and allows an index-only scan.
CREATE INDEX IF NOT EXISTS channels_enabled_idx
    ON channels(id)
    WHERE enabled;

-- Covering index for the groups aggregation query.
-- The query groups by group_name and counts enabled channels.
-- This index on (group_name, id) WHERE enabled allows an index-only scan
-- for the aggregation instead of a full table scan.
CREATE INDEX IF NOT EXISTS channels_group_enabled_idx
    ON channels(group_name)
    INCLUDE (id)
    WHERE enabled;

-- Covering index for healthy stream count via EXISTS.
-- The query uses EXISTS against channel_streams(channel_id) which is
-- already indexed by channel_streams_channel_id_idx from migration 0005.
-- No additional index is needed; the query rewrite to EXISTS avoids the
-- expensive count(DISTINCT) with external merge sort.

-- Index for programmes query by channel_id through channel_epg_mappings.
-- The programmes listing query joins programmes -> epg_channels ->
-- channel_epg_mappings and filters by m.channel_id.
-- This index on channel_epg_mappings(epg_channel_id) supports the join
-- from programmes to mappings.
CREATE INDEX IF NOT EXISTS channel_epg_mappings_epg_channel_idx
    ON channel_epg_mappings(epg_channel_id);

-- Update planner statistics after structural changes.
ANALYZE channels;
ANALYZE channel_streams;
ANALYZE channel_epg_mappings;
ANALYZE provider_streams;
ANALYZE programmes;
ANALYZE epg_channels;
ANALYZE source_snapshots;

-- Tune autovacuum for large tables to keep visibility maps fresh.
-- Index-only scans require all-visible pages. Large tables that receive
-- frequent updates (reconciliation) need more aggressive autovacuum to
-- keep the visibility map current so count queries stay fast.
ALTER TABLE channels SET (
    autovacuum_vacuum_scale_factor = 0.05,
    autovacuum_analyze_scale_factor = 0.02
);
ALTER TABLE channel_streams SET (
    autovacuum_vacuum_scale_factor = 0.05,
    autovacuum_analyze_scale_factor = 0.02
);
ALTER TABLE provider_streams SET (
    autovacuum_vacuum_scale_factor = 0.05,
    autovacuum_analyze_scale_factor = 0.02
);
ALTER TABLE programmes SET (
    autovacuum_vacuum_scale_factor = 0.05,
    autovacuum_analyze_scale_factor = 0.02
);
