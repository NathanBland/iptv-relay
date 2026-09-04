-- Update planner statistics after concurrent index creation.
-- These are safe to run inside a transaction and are fast.
ANALYZE source_snapshots;
ANALYZE programmes;
ANALYZE epg_channels;
ANALYZE channel_epg_mappings;
ANALYZE jobs;
ANALYZE audit_events;
ANALYZE provider_streams;
