-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_events_resource_id_idx
    ON audit_events(resource_id);
