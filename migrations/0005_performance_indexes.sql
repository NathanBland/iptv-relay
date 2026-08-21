-- Index for channel list ordering by channel_number with keyset pagination.
CREATE INDEX IF NOT EXISTS channels_channel_number_sort_idx
    ON channels(channel_number);

-- Index for group filtering and group listing.
CREATE INDEX IF NOT EXISTS channels_group_name_idx
    ON channels(group_name)
    WHERE group_name IS NOT NULL;

-- Covering index for channel list page (id for join, enabled for filter).
CREATE INDEX IF NOT EXISTS channels_list_covering_idx
    ON channels(channel_number)
    INCLUDE (id, enabled);

-- Index for EPG mapping existence checks (left join optimization).
CREATE INDEX IF NOT EXISTS channel_epg_mappings_channel_idx
    ON channel_epg_mappings(channel_id);

-- Index for channel stream counts by channel.
CREATE INDEX IF NOT EXISTS channel_streams_channel_id_idx
    ON channel_streams(channel_id);

-- Index for programmes listing by channel and time.
CREATE INDEX IF NOT EXISTS programmes_epg_channel_starts_idx
    ON programmes(epg_channel_id, starts_at DESC);

-- Index for provider streams by snapshot and supported flag (reconciliation).
CREATE INDEX IF NOT EXISTS provider_streams_snapshot_supported_idx
    ON provider_streams(snapshot_id)
    WHERE supported;
