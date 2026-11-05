-- Key event channels by template and target channel, not by slot number.
-- The scan attaches each event to the channel that carries the matched
-- provider stream. The slot_number column stays for compatibility.

ALTER TABLE event_channels
    DROP CONSTRAINT IF EXISTS event_channels_template_id_slot_number_key;

-- Remove duplicate rows that the slot model attached to one channel.
-- Keep the most recently updated row for each template and channel.
DELETE FROM event_channels older
USING event_channels newer
WHERE older.template_id = newer.template_id
  AND older.channel_id = newer.channel_id
  AND (older.updated_at, older.id) < (newer.updated_at, newer.id);

CREATE UNIQUE INDEX IF NOT EXISTS event_channels_template_channel_idx
    ON event_channels(template_id, channel_id);

-- Stable output name of a channel with a parsed event, for example "NHL 01".
-- The event scan writes this column. Provider reconciliation does not.
ALTER TABLE channels ADD COLUMN IF NOT EXISTS event_name text;

COMMENT ON COLUMN channels.event_name IS
    'Stable output name from the event scan. Precedence: name_override, alias, event_name, name.';

-- The matchRegex value of a template uses the Rust regex syntax. Change the
-- PostgreSQL word-boundary escapes to the Rust escape \b.
UPDATE event_templates
SET match_regex = replace(replace(replace(match_regex, '\y', '\b'), '\m', '\b'), '\M', '\b'),
    updated_at = now()
WHERE strpos(match_regex, '\y') > 0
   OR strpos(match_regex, '\m') > 0
   OR strpos(match_regex, '\M') > 0;

-- Built-in template: apply the built-in sports rules to all enabled channels.
-- The operator can disable or delete it. The scan applies it after all
-- operator templates.
INSERT INTO event_templates
    (id, name, display_name, match_regex, channel_name_format, group_name,
     event_duration_hours, past_date_grace_hours, future_date_days, timezone,
     filler_title, enabled)
VALUES
    ('8b2d571e-91de-7000-8000-00000000e000', 'built-in-sports', 'Built-in sports',
     '.*', '{event}', 'Sports', 3, 4, 7, 'UTC', 'No programs available', true)
ON CONFLICT DO NOTHING;
