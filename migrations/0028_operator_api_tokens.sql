-- Durable operator API credentials.
-- Store only one-way hashes. The plaintext value exists only in the create or
-- rotate response and is never written to the database.
CREATE TABLE IF NOT EXISTS operator_api_tokens (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    token_hash bytea NOT NULL UNIQUE,
    scopes text[] NOT NULL CHECK (cardinality(scopes) > 0),
    expires_at timestamptz,
    revoked_at timestamptz,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);

CREATE INDEX IF NOT EXISTS operator_api_tokens_active_idx
    ON operator_api_tokens (created_at DESC)
    WHERE revoked_at IS NULL;

CREATE INDEX IF NOT EXISTS operator_api_tokens_hash_idx
    ON operator_api_tokens (token_hash)
    WHERE revoked_at IS NULL;
