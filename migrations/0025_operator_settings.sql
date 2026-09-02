-- Stores the current operator overrides for global, provider, and channel
-- group scopes. Each row is one setting key with its effective JSON value.
-- The revision column mirrors the latest revision recorded for the scope in
-- the shared revisions table so ETag and If-Match conflict checks stay cheap.
CREATE TABLE IF NOT EXISTS operator_setting_overrides (
    scope text NOT NULL CHECK (scope IN ('global', 'provider', 'group')),
    scope_id text NOT NULL DEFAULT '',
    key text NOT NULL,
    value jsonb NOT NULL,
    revision bigint NOT NULL DEFAULT 1,
    updated_by text NOT NULL DEFAULT 'operator',
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, scope_id, key)
);

COMMENT ON TABLE operator_setting_overrides IS
    'This table stores the current operator overrides for global, provider, and channel group scopes. The revision column mirrors the latest revisions row for the scope.';

CREATE INDEX IF NOT EXISTS operator_setting_overrides_scope_idx
    ON operator_setting_overrides(scope, scope_id);
