-- DVR recording scheduling support.
-- Adds recording rules, recording instances, and recording storage tracking.

-- Recording rules define what to record.
CREATE TABLE IF NOT EXISTS recording_rules (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    rule_type text NOT NULL DEFAULT 'one-time'
        CHECK (rule_type IN ('one-time', 'recurring', 'series')),
    title_filter text,
    category_filter text,
    start_padding_minutes integer NOT NULL DEFAULT 0 CHECK (start_padding_minutes >= 0),
    end_padding_minutes integer NOT NULL DEFAULT 0 CHECK (end_padding_minutes >= 0),
    max_recordings integer,
    keep_until text NOT NULL DEFAULT 'space-needed'
        CHECK (keep_until IN ('space-needed', 'one-day', 'one-week', 'until-watched', 'forever')),
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS recording_rules_channel_idx ON recording_rules(channel_id);
CREATE INDEX IF NOT EXISTS recording_rules_enabled_idx ON recording_rules(enabled) WHERE enabled = true;

-- Recording instances represent scheduled or completed recordings.
CREATE TABLE IF NOT EXISTS recordings (
    id uuid PRIMARY KEY,
    rule_id uuid REFERENCES recording_rules(id) ON DELETE SET NULL,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    programme_id uuid,
    title text NOT NULL,
    description text,
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'recording', 'completed', 'failed', 'cancelled')),
    file_path text,
    file_size_bytes bigint,
    duration_seconds integer,
    error_message text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS recordings_channel_idx ON recordings(channel_id, starts_at);
CREATE INDEX IF NOT EXISTS recordings_status_idx ON recordings(status);
CREATE INDEX IF NOT EXISTS recordings_rule_idx ON recordings(rule_id) WHERE rule_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS recordings_scheduled_idx ON recordings(starts_at)
    WHERE status = 'scheduled';
