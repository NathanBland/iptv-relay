-- Bitrate, burst and frame rate measured from live viewer sessions.
--
-- A provider refresh inserts new provider_streams rows, so values stored on
-- those rows are lost at each refresh. These rows use the provider account and
-- the stable stream key, which stay the same across refreshes.
CREATE TABLE IF NOT EXISTS stream_measurements (
    provider_account_id uuid NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    stable_key text NOT NULL,
    average_bitrate_kbps integer NOT NULL CHECK (average_bitrate_kbps > 0),
    peak_bitrate_kbps integer NOT NULL CHECK (peak_bitrate_kbps > 0),
    burst_ms integer NOT NULL CHECK (burst_ms >= 0),
    frame_rate real CHECK (frame_rate IS NULL OR frame_rate > 0),
    samples integer NOT NULL DEFAULT 1,
    measured_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider_account_id, stable_key)
);
