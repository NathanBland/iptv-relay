CREATE TABLE IF NOT EXISTS generated_programmes (
    id uuid PRIMARY KEY,
    event_channel_id uuid NOT NULL REFERENCES event_channels(id) ON DELETE CASCADE,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    template_id uuid NOT NULL REFERENCES event_templates(id) ON DELETE CASCADE,
    rule_id uuid NOT NULL,
    rule_name text NOT NULL,
    stable_event_key text,
    source_title text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('event', 'filler')),
    starts_at timestamptz NOT NULL,
    stops_at timestamptz NOT NULL,
    title text NOT NULL,
    categories jsonb NOT NULL DEFAULT '[]'::jsonb,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (stops_at > starts_at)
);

CREATE UNIQUE INDEX IF NOT EXISTS generated_programmes_event_key_idx
    ON generated_programmes(event_channel_id, stable_event_key)
    WHERE stable_event_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS generated_programmes_channel_time_idx
    ON generated_programmes(channel_id, starts_at, stops_at);

CREATE EXTENSION IF NOT EXISTS btree_gist;

ALTER TABLE generated_programmes
    ADD CONSTRAINT generated_programmes_channel_interval_excl
    EXCLUDE USING gist (
        channel_id WITH =,
        tstzrange(starts_at, stops_at, '[)') WITH &&
    );
