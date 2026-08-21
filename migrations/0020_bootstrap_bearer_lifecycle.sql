CREATE TABLE IF NOT EXISTS authentication_state (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    bootstrap_bearer_enabled boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO authentication_state (id, bootstrap_bearer_enabled)
VALUES (true, true)
ON CONFLICT (id) DO NOTHING;
