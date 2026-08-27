-- Stores the user's IANA timezone and enabled region prefixes.
-- It controls the region prefix filter and the auto-detected state.
CREATE TABLE IF NOT EXISTS region_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    timezone TEXT NOT NULL DEFAULT 'America/Denver',
    enabled_prefixes TEXT[] NOT NULL DEFAULT '{}',
    auto_detected BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE region_settings IS 'This table stores the user''s IANA timezone and enabled region prefixes. It controls the region prefix filter and the auto-detected state.';

INSERT INTO region_settings (id, timezone, enabled_prefixes, auto_detected)
VALUES (
    '00000000-0000-0000-0000-000000000000',
    'America/Denver',
    ARRAY['US','USA','CAN','EN','LA','GLOBAL','MULTI','SPT'],
    true
)
ON CONFLICT (id) DO NOTHING;
