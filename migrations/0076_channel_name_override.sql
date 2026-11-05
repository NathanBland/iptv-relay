-- Operator name override for a channel. Provider reconciliation does not
-- write this column, so the override survives each refresh.

ALTER TABLE channels ADD COLUMN IF NOT EXISTS name_override text;
