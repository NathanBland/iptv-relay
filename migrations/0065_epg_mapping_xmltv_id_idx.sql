-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS epg_channels_snapshot_lower_xmltv_idx ON epg_channels (source_snapshot_id, lower(xmltv_id));
