-- Stream health checking and quality ranking support.
-- Adds health check columns to provider_streams and a stream check log table.

-- Add health check columns to provider_streams.
ALTER TABLE provider_streams
    ADD COLUMN IF NOT EXISTS health_status text NOT NULL DEFAULT 'unknown'
        CHECK (health_status IN ('alive', 'dead', 'unknown', 'checking')),
    ADD COLUMN IF NOT EXISTS health_checked_at timestamptz,
    ADD COLUMN IF NOT EXISTS health_error text,
    ADD COLUMN IF NOT EXISTS video_codec text,
    ADD COLUMN IF NOT EXISTS video_resolution text,
    ADD COLUMN IF NOT EXISTS video_width integer,
    ADD COLUMN IF NOT EXISTS video_height integer,
    ADD COLUMN IF NOT EXISTS video_fps real,
    ADD COLUMN IF NOT EXISTS audio_codec text,
    ADD COLUMN IF NOT EXISTS audio_channels integer,
    ADD COLUMN IF NOT EXISTS audio_sample_rate integer,
    ADD COLUMN IF NOT EXISTS bitrate_kbps integer;

-- Index for finding streams that need health checks.
CREATE INDEX IF NOT EXISTS provider_streams_health_status_idx
    ON provider_streams(health_status)
    WHERE health_status IN ('unknown', 'dead');

-- Index for quality ranking lookups.
CREATE INDEX IF NOT EXISTS provider_streams_quality_idx
    ON provider_streams(video_width, video_height, video_fps DESC)
    WHERE health_status = 'alive';

-- Stream health check log for audit and scheduling.
CREATE TABLE IF NOT EXISTS stream_health_checks (
    id uuid PRIMARY KEY,
    provider_stream_id uuid NOT NULL REFERENCES provider_streams(id) ON DELETE CASCADE,
    status text NOT NULL CHECK (status IN ('alive', 'dead', 'error')),
    error_message text,
    video_codec text,
    video_resolution text,
    video_width integer,
    video_height integer,
    video_fps real,
    audio_codec text,
    audio_channels integer,
    audio_sample_rate integer,
    bitrate_kbps integer,
    check_duration_ms integer,
    checked_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS stream_health_checks_stream_idx
    ON stream_health_checks(provider_stream_id, checked_at DESC);

-- Add failover tracking to channel_streams.
ALTER TABLE channel_streams
    ADD COLUMN IF NOT EXISTS failover_count integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_failover_at timestamptz;

-- Add a quality rank column to channel_streams for stream quality ordering.
-- Lower rank value means higher priority (best quality first).
ALTER TABLE channel_streams
    ADD COLUMN IF NOT EXISTS quality_rank integer NOT NULL DEFAULT 0;

-- Index for failover queries.
CREATE INDEX IF NOT EXISTS channel_streams_failover_idx
    ON channel_streams(channel_id, priority, quality_rank)
    WHERE priority >= 0;
