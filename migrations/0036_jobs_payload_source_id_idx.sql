-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS jobs_payload_source_id_idx
    ON jobs ((payload->>'sourceId'))
    WHERE payload ? 'sourceId';
