-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS channels_region_prefix_enabled_idx
    ON channels (enabled, (substring(group_name FROM '^([A-Z]{2,7}):')))
    WHERE group_name IS NOT NULL
      AND group_name ~ '^[A-Z]{2,7}:';
