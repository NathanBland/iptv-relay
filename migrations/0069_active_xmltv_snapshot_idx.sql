-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS source_snapshots_active_xmltv_idx
    ON source_snapshots (activated_at DESC NULLS LAST, id DESC)
    WHERE kind = 'xmltv' AND status = 'active';
