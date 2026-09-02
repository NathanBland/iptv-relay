-- Store guide settings that apply to all events in a dynamic channel group.
ALTER TABLE event_templates
    ADD COLUMN IF NOT EXISTS timezone TEXT NOT NULL DEFAULT 'UTC',
    ADD COLUMN IF NOT EXISTS filler_title TEXT NOT NULL DEFAULT 'No programs available';

COMMENT ON COLUMN event_templates.timezone IS
    'IANA timezone for local event timestamps in provider stream titles.';
COMMENT ON COLUMN event_templates.filler_title IS
    'Programme title for guide intervals without a detected event.';
