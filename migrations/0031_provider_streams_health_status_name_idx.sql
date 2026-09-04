-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS provider_streams_health_status_name_idx
    ON provider_streams(health_status, name)
    WHERE supported = true;
