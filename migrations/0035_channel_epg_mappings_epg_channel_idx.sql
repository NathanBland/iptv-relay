-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channel_epg_mappings_epg_channel_idx
    ON channel_epg_mappings(epg_channel_id);
