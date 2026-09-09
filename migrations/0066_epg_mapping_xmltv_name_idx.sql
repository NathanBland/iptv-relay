-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS epg_channels_snapshot_normalized_name_idx ON epg_channels (source_snapshot_id, normalize_channel_name(display_names->0->>'value'));
