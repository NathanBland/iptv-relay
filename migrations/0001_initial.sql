CREATE TABLE connection_pools (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    max_connections integer NOT NULL CHECK (max_connections > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE provider_accounts (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    source_type text NOT NULL CHECK (source_type IN ('m3u', 'xtream')),
    base_url_template text NOT NULL,
    secret_ciphertext bytea,
    connection_pool_id uuid REFERENCES connection_pools(id) ON DELETE SET NULL,
    max_connections integer NOT NULL DEFAULT 1 CHECK (max_connections > 0),
    input_adapter text NOT NULL DEFAULT 'auto'
        CHECK (input_adapter IN ('auto', 'native-ts', 'ffmpeg', 'vlc')),
    source_timezone text NOT NULL DEFAULT 'UTC',
    enabled boolean NOT NULL DEFAULT true,
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE source_snapshots (
    id uuid PRIMARY KEY,
    provider_account_id uuid NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('m3u', 'xtream', 'xmltv')),
    status text NOT NULL CHECK (status IN ('staging', 'active', 'rejected', 'superseded')),
    checksum_sha256 text NOT NULL,
    byte_count bigint NOT NULL CHECK (byte_count >= 0),
    record_count bigint NOT NULL DEFAULT 0 CHECK (record_count >= 0),
    diagnostic_count bigint NOT NULL DEFAULT 0 CHECK (diagnostic_count >= 0),
    diagnostics jsonb NOT NULL DEFAULT '[]'::jsonb,
    fetched_at timestamptz NOT NULL DEFAULT now(),
    activated_at timestamptz,
    UNIQUE (provider_account_id, kind, checksum_sha256)
);

CREATE UNIQUE INDEX one_active_snapshot_per_provider_kind
    ON source_snapshots(provider_account_id, kind)
    WHERE status = 'active';

CREATE TABLE provider_streams (
    id uuid PRIMARY KEY,
    snapshot_id uuid NOT NULL REFERENCES source_snapshots(id) ON DELETE CASCADE,
    provider_account_id uuid NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    stable_key text NOT NULL,
    provider_stream_id text,
    name text NOT NULL,
    group_name text,
    tvg_id text,
    tvg_name text,
    logo_url text,
    channel_number text,
    url_template text NOT NULL,
    url_secret_ciphertext bytea,
    attributes jsonb NOT NULL DEFAULT '{}'::jsonb,
    directives jsonb NOT NULL DEFAULT '[]'::jsonb,
    supported boolean NOT NULL DEFAULT true,
    health jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (snapshot_id, stable_key)
);

CREATE INDEX provider_streams_account_stable_key_idx
    ON provider_streams(provider_account_id, stable_key);
CREATE INDEX provider_streams_tvg_id_idx
    ON provider_streams(provider_account_id, tvg_id)
    WHERE tvg_id IS NOT NULL AND tvg_id <> '';
CREATE INDEX provider_streams_group_idx ON provider_streams(group_name);

CREATE TABLE channels (
    id uuid PRIMARY KEY,
    channel_number text NOT NULL,
    name text NOT NULL,
    group_name text,
    logo_url text,
    enabled boolean NOT NULL DEFAULT true,
    managed_by text NOT NULL DEFAULT 'automatic'
        CHECK (managed_by IN ('automatic', 'manual', 'event')),
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (channel_number)
);

CREATE TABLE channel_streams (
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    provider_stream_id uuid NOT NULL REFERENCES provider_streams(id) ON DELETE CASCADE,
    priority integer NOT NULL CHECK (priority >= 0),
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    PRIMARY KEY (channel_id, provider_stream_id),
    UNIQUE (channel_id, priority)
);

CREATE TABLE epg_sources (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    url_template text NOT NULL,
    secret_ciphertext bytea,
    timezone text NOT NULL DEFAULT 'UTC',
    priority integer NOT NULL DEFAULT 100,
    enabled boolean NOT NULL DEFAULT true,
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE epg_channels (
    id uuid PRIMARY KEY,
    source_snapshot_id uuid NOT NULL REFERENCES source_snapshots(id) ON DELETE CASCADE,
    epg_source_id uuid NOT NULL REFERENCES epg_sources(id) ON DELETE CASCADE,
    xmltv_id text NOT NULL,
    display_names jsonb NOT NULL DEFAULT '[]'::jsonb,
    icon_urls jsonb NOT NULL DEFAULT '[]'::jsonb,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (source_snapshot_id, xmltv_id)
);

CREATE INDEX epg_channels_source_xmltv_idx ON epg_channels(epg_source_id, xmltv_id);

CREATE TABLE programmes (
    id uuid PRIMARY KEY,
    source_snapshot_id uuid NOT NULL REFERENCES source_snapshots(id) ON DELETE CASCADE,
    epg_channel_id uuid NOT NULL REFERENCES epg_channels(id) ON DELETE CASCADE,
    starts_at timestamptz NOT NULL,
    stops_at timestamptz,
    original_start text NOT NULL,
    original_stop text,
    title text NOT NULL,
    subtitle text,
    description text,
    categories jsonb NOT NULL DEFAULT '[]'::jsonb,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE UNIQUE INDEX programmes_exact_dedupe_idx
    ON programmes(epg_channel_id, starts_at, COALESCE(stops_at, 'infinity'::timestamptz), title);
CREATE INDEX programmes_channel_time_idx ON programmes(epg_channel_id, starts_at, stops_at);

CREATE TABLE channel_epg_mappings (
    channel_id uuid PRIMARY KEY REFERENCES channels(id) ON DELETE CASCADE,
    epg_channel_id uuid NOT NULL REFERENCES epg_channels(id) ON DELETE CASCADE,
    method text NOT NULL CHECK (method IN ('manual', 'previous', 'tvg-id', 'callsign', 'alias', 'exact-name', 'fuzzy', 'generated')),
    confidence real NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    revision bigint NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE event_rule_sets (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    group_selector jsonb NOT NULL,
    rules jsonb NOT NULL,
    source_timezone text NOT NULL DEFAULT 'UTC',
    default_duration_seconds integer NOT NULL DEFAULT 10800 CHECK (default_duration_seconds > 0),
    filler_title text NOT NULL DEFAULT 'No programs available',
    enabled boolean NOT NULL DEFAULT true,
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE output_profiles (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    token_hash bytea NOT NULL UNIQUE,
    previous_token_hash bytea,
    previous_token_expires_at timestamptz,
    tuner_count integer NOT NULL DEFAULT 1 CHECK (tuner_count > 0),
    enabled boolean NOT NULL DEFAULT true,
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE output_profile_channels (
    output_profile_id uuid NOT NULL REFERENCES output_profiles(id) ON DELETE CASCADE,
    channel_id uuid NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    position integer NOT NULL CHECK (position >= 0),
    PRIMARY KEY (output_profile_id, channel_id),
    UNIQUE (output_profile_id, position)
);

CREATE TABLE jobs (
    id uuid PRIMARY KEY,
    kind text NOT NULL,
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    priority integer NOT NULL DEFAULT 0,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    progress jsonb NOT NULL DEFAULT '{}'::jsonb,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 3 CHECK (max_attempts > 0),
    available_at timestamptz NOT NULL DEFAULT now(),
    locked_by text,
    locked_at timestamptz,
    heartbeat_at timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz
);

CREATE INDEX jobs_claim_idx ON jobs(priority DESC, created_at)
    WHERE status = 'queued';
CREATE INDEX jobs_running_heartbeat_idx ON jobs(heartbeat_at)
    WHERE status = 'running';

CREATE TABLE revisions (
    id uuid PRIMARY KEY,
    resource_type text NOT NULL,
    resource_id uuid NOT NULL,
    revision bigint NOT NULL,
    actor text NOT NULL,
    before_value jsonb,
    after_value jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (resource_type, resource_id, revision)
);

CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    actor text NOT NULL,
    action text NOT NULL,
    resource_type text NOT NULL,
    resource_id uuid,
    correlation_id uuid NOT NULL,
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_created_idx ON audit_events(created_at DESC);

