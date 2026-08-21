-- Event templates define rules for a league (NFL, NBA, MLB, NHL, etc.)
CREATE TABLE IF NOT EXISTS event_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    match_regex TEXT NOT NULL,
    channel_name_format TEXT NOT NULL,
    group_name TEXT NOT NULL,
    event_duration_hours INT NOT NULL DEFAULT 3,
    past_date_grace_hours INT NOT NULL DEFAULT 4,
    future_date_days INT NOT NULL DEFAULT 2,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Event channels are auto-created from templates when events are detected
CREATE TABLE IF NOT EXISTS event_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES event_templates(id) ON DELETE CASCADE,
    channel_id UUID REFERENCES channels(id) ON DELETE SET NULL,
    slot_number INT NOT NULL,
    event_title TEXT,
    event_start TIMESTAMPTZ,
    event_end TIMESTAMPTZ,
    raw_stream_name TEXT,
    state TEXT NOT NULL DEFAULT 'scheduled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(template_id, slot_number)
);

CREATE INDEX IF NOT EXISTS event_channels_template_idx ON event_channels(template_id);
CREATE INDEX IF NOT EXISTS event_channels_state_idx ON event_channels(state);
CREATE INDEX IF NOT EXISTS event_channels_channel_idx ON event_channels(channel_id);
