-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS jobs_refresh_source_latest_idx
    ON jobs ((payload->>'sourceId'), created_at DESC)
    INCLUDE (status)
    WHERE kind = 'refresh-source';
