-- Multi-user access control support.
-- Adds user accounts with granular permissions and per-user channel access.

-- User accounts with roles and permissions.
CREATE TABLE IF NOT EXISTS users (
    id uuid PRIMARY KEY,
    username text NOT NULL UNIQUE,
    display_name text NOT NULL,
    password_hash text NOT NULL,
    role text NOT NULL DEFAULT 'viewer'
        CHECK (role IN ('admin', 'operator', 'viewer')),
    enabled boolean NOT NULL DEFAULT true,
    last_login_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Per-user channel access grants.
-- If no grants exist for a user, the user has access to all enabled channels.
-- If grants exist, the user has access only to the granted channels.
CREATE TABLE IF NOT EXISTS user_channel_grants (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    granted_at timestamptz NOT NULL DEFAULT now(),
    granted_by uuid REFERENCES users(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, channel_id)
);

CREATE INDEX IF NOT EXISTS user_channel_grants_user_idx
    ON user_channel_grants(user_id);

-- Per-user output profile access.
-- If no grants exist, the user has access to all enabled output profiles.
CREATE TABLE IF NOT EXISTS user_profile_grants (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    output_profile_id uuid NOT NULL REFERENCES output_profiles(id) ON DELETE CASCADE,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, output_profile_id)
);

CREATE INDEX IF NOT EXISTS user_profile_grants_user_idx
    ON user_profile_grants(user_id);

-- API tokens for user authentication.
CREATE TABLE IF NOT EXISTS user_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    name text NOT NULL,
    last_used_at timestamptz,
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS user_tokens_user_idx ON user_tokens(user_id);
CREATE INDEX IF NOT EXISTS user_tokens_hash_idx ON user_tokens(token_hash);
