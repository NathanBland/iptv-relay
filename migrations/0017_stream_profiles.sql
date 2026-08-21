-- Stream profiles for upstream connection configuration.
-- Allows configuring how the backend connects to provider streams.

CREATE TABLE IF NOT EXISTS stream_profiles (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    profile_type text NOT NULL DEFAULT 'direct'
        CHECK (profile_type IN ('direct', 'ffmpeg', 'vlc', 'streamlink', 'custom')),
    command text,
    arguments jsonb NOT NULL DEFAULT '[]'::jsonb,
    buffer_seconds real NOT NULL DEFAULT 0 CHECK (buffer_seconds >= 0),
    user_agent text,
    referer text,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Default profiles.
INSERT INTO stream_profiles (id, name, profile_type, buffer_seconds) VALUES
    ('22222222-0000-0000-0000-000000000001', 'Direct', 'direct', 0),
    ('22222222-0000-0000-0000-000000000002', 'FFmpeg', 'ffmpeg', 3),
    ('22222222-0000-0000-0000-000000000003', 'VLC', 'vlc', 5),
    ('22222222-0000-0000-0000-000000000004', 'Streamlink', 'streamlink', 3)
ON CONFLICT (name) DO NOTHING;

-- Assign a stream profile to a channel.
CREATE TABLE IF NOT EXISTS channel_stream_profiles (
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    stream_profile_id uuid NOT NULL REFERENCES stream_profiles(id) ON DELETE CASCADE,
    assigned_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, stream_profile_id)
);

CREATE INDEX IF NOT EXISTS channel_stream_profiles_channel_idx
    ON channel_stream_profiles(channel_id);
