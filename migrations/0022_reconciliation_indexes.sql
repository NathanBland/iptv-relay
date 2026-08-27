-- Functional index for canonical key lookups used by reconciliation joins.
-- The expression COALESCE(NULLIF(tvg_id, ''), 'stream:' || stable_key) is
-- used in channel upserts, channel_streams links, and orphan detection.
CREATE INDEX IF NOT EXISTS provider_streams_snapshot_canonical_key_idx
    ON provider_streams (snapshot_id, COALESCE(NULLIF(tvg_id, ''), 'stream:' || stable_key))
    WHERE supported;

-- Partial index for managed automatic channels keyed by provider account and
-- canonical key. Used by orphan DELETE anti-joins and channel number conflict
-- detection during reconciliation.
CREATE INDEX IF NOT EXISTS channels_provider_canonical_key_managed_idx
    ON channels (provider_account_id, canonical_key)
    WHERE provider_account_id IS NOT NULL
      AND canonical_key IS NOT NULL
      AND managed_by = 'automatic';

-- Update planner statistics after adding functional indexes.
ANALYZE provider_streams;
ANALYZE channels;
