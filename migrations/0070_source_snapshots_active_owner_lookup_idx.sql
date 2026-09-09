-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS source_snapshots_active_owner_lookup_idx
    ON source_snapshots ((COALESCE(provider_account_id, epg_source_id)), activated_at DESC NULLS LAST)
    INCLUDE (record_count)
    WHERE status = 'active';
