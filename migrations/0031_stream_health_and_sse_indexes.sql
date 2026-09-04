-- Covering index for the stream health page query.
-- list_stream_health orders by health_status, name with an optional
-- supported filter. This index lets the planner satisfy the ORDER BY
-- without a sort step on the 6.8M-row provider_streams table.
CREATE INDEX IF NOT EXISTS provider_streams_health_status_name_idx
    ON provider_streams(health_status, name)
    WHERE supported = true;

-- Update planner statistics after adding new indexes.
ANALYZE provider_streams;
