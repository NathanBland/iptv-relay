-- Add EPG mapping review workflow and manual override support.
-- review_status tracks the lifecycle of an auto-matched mapping:
--   applied  - auto-matched and active
--   review   - needs operator review before activation
--   rejected - operator rejected the auto-match
--   manual   - operator set this mapping manually
-- reviewed_by and reviewed_at record who reviewed the mapping and when.
-- The review_candidates table stores alternative EPG channel candidates
-- for channels that need operator review.

ALTER TABLE channel_epg_mappings
    ADD COLUMN IF NOT EXISTS review_status text NOT NULL DEFAULT 'applied'
        CHECK (review_status IN ('applied', 'review', 'rejected', 'manual')),
    ADD COLUMN IF NOT EXISTS reviewed_by text,
    ADD COLUMN IF NOT EXISTS reviewed_at timestamptz;

CREATE INDEX IF NOT EXISTS channel_epg_mappings_review_status_idx
    ON channel_epg_mappings (review_status);

CREATE TABLE IF NOT EXISTS review_candidates (
    id uuid PRIMARY KEY,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    epg_channel_id uuid NOT NULL REFERENCES epg_channels(id) ON DELETE CASCADE,
    method text NOT NULL CHECK (method IN ('manual', 'previous', 'tvg-id', 'callsign', 'alias', 'exact-name', 'fuzzy', 'generated')),
    confidence real NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (channel_id, epg_channel_id)
);

CREATE INDEX IF NOT EXISTS review_candidates_channel_id_idx
    ON review_candidates (channel_id);
