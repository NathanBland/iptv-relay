-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channels_region_prefix_group_idx
    ON channels ((substring(group_name FROM '^([A-Z]{2,7}):')), group_name)
    INCLUDE (enabled)
    WHERE group_name IS NOT NULL
      AND group_name ~ '^[A-Z]{2,7}:';
