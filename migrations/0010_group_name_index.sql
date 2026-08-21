-- Add a non-partial index on group_name for efficient group-level updates.
-- The existing channels_group_enabled_idx is a partial index (WHERE enabled)
-- which cannot serve queries that filter on disabled rows.
-- This index enables fast individual group enable/disable operations
-- and supports the bulk set_all_groups_enabled query.
CREATE INDEX IF NOT EXISTS channels_group_name_idx
    ON channels (group_name);
