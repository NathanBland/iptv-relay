-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS jobs_open_source_refresh_unique_idx ON jobs ((payload->>'sourceId')) WHERE kind = 'refresh-source' AND status IN ('queued', 'running');
