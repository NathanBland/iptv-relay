//! HTTP control and Jellyfin-facing output APIs.

mod auth;
mod oidc;

pub use auth::hash_admin_password;
pub use oidc::OidcConfig;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    convert::Infallible,
    fmt::{self, Write as _},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use iptv_domain::{
    ApplyRequirement, EffectiveSetting, InheritanceSource, SettingDefinition,
    SettingValidationError, resolve_settings, setting_catalog,
};
use iptv_media::{
    AcquireError, HttpTsEndpoint, HttpTsSessionKey, HttpTsSessionManager, HttpTsSessionSnapshot,
    HttpTsSourceSpec, InputAdapterPolicy, MpegTsRingConfig, PoolSnapshot, ProviderSpec,
    SessionFailureKind, SessionStartError, SessionState, SlotLease, ViewerHandle,
};
use iptv_persistence::{
    AuditEventRecord, CatalogRepository, ChannelPlaybackCandidateRow, ChannelPlaybackPlan,
    ChannelQuery, CreateEventTemplate, Database, EventTemplateQuery, EventTemplateUpdate,
    JobRecord, JobRepository, LineupApplyStats, LineupCategoryRow, LineupChannelRow,
    LineupTemplateRow, MasterKey, NewSource, OperatorSettingScopeState, OutputProfileRow,
    OutputProfileTokenHash, PersistenceError, ProgrammeQuery, SourceKind, SourceRepository,
    SourceSummary, redact_diagnostics, redact_error,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tower_http::{catch_panic::CatchPanicLayer, compression::CompressionLayer};
use utoipa::{IntoParams, OpenApi, ToSchema};
use uuid::Uuid;

use crate::auth::{
    AuthManager, LoginError, OPERATOR_TOKEN_ADMIN_SCOPE, OPERATOR_TOKEN_CONTROL_SCOPE,
    OPERATOR_TOKEN_OUTPUT_SCOPE, OPERATOR_TOKEN_READ_SCOPE, OperatorTokenRecord,
};
use crate::oidc::OidcError;

const OPERATOR_SETTING_REVISION_PAGE_SIZE: i64 = 500;

#[derive(Clone)]
pub struct AppConfig {
    pub public_base_url: String,
    pub output_token: String,
    pub admin_bootstrap_token: String,
    pub admin_password_hash: String,
    pub master_key: MasterKey,
    pub tuner_count: u16,
    pub runtime_versions: RuntimeVersions,
    pub oidc: Option<OidcConfig>,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppConfig")
            .field("public_base_url", &self.public_base_url)
            .field("output_token", &"<redacted>")
            .field("admin_bootstrap_token", &"<redacted>")
            .field("admin_password_hash", &"<redacted>")
            .field("master_key", &self.master_key)
            .field("tuner_count", &self.tuner_count)
            .field("runtime_versions", &self.runtime_versions)
            .field("oidc", &self.oidc)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub struct RuntimeVersions {
    pub gateway: String,
    pub ffmpeg: Option<String>,
    pub vlc: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    database: Option<Database>,
    catalog: Arc<RwLock<CatalogSnapshot>>,
    public_base_url: Arc<str>,
    output_token_hash: [u8; 32],
    environment_output_token: Arc<str>,
    auth: AuthManager,
    tuner_count: u16,
    runtime_versions: RuntimeVersions,
    started_at: Instant,
    media: HttpTsSessionManager,
    internal_reservations: Arc<std::sync::Mutex<HashMap<String, (SlotLease, Instant)>>>,
    internal_token_hash: [u8; 32],
    source_repository: Option<SourceRepository>,
    job_repository: Option<JobRepository>,
    catalog_repository: Option<CatalogRepository>,
    jellyfin_setup: Arc<RwLock<JellyfinSetup>>,
    master_key: MasterKey,
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("database", &self.database)
            .field("catalog", &self.catalog)
            .field("public_base_url", &self.public_base_url)
            .field("output_token_hash", &"<redacted>")
            .field("environment_output_token", &"<redacted>")
            .field("auth", &self.auth)
            .field("tuner_count", &self.tuner_count)
            .field("runtime_versions", &self.runtime_versions)
            .field("started_at", &self.started_at)
            .field("media", &self.media)
            .field("internal_reservations", &"<redacted>")
            .field("internal_token_hash", &"<redacted>")
            .field("source_repository", &self.source_repository)
            .field("job_repository", &self.job_repository)
            .field("catalog_repository", &self.catalog_repository)
            .field("jellyfin_setup", &self.jellyfin_setup)
            .field("master_key", &self.master_key)
            .finish()
    }
}

impl AppState {
    pub fn new(database: Option<Database>, config: AppConfig) -> Self {
        let secure_cookies = url::Url::parse(config.public_base_url.trim())
            .is_ok_and(|url| url.scheme().eq_ignore_ascii_case("https"));
        let source_repository = database.as_ref().map(|database| {
            SourceRepository::new(database.pool().clone(), config.master_key.clone())
        });
        let job_repository = database
            .as_ref()
            .map(|database| JobRepository::new(database.pool().clone()));
        let catalog_repository = database
            .as_ref()
            .map(|database| CatalogRepository::new(database.pool().clone()));
        Self {
            database,
            catalog: Arc::new(RwLock::new(CatalogSnapshot::default())),
            public_base_url: config.public_base_url.trim_end_matches('/').into(),
            output_token_hash: token_hash(&config.output_token),
            environment_output_token: Arc::from(config.output_token.as_str()),
            auth: AuthManager::new_with_oidc(
                config.admin_password_hash,
                &config.admin_bootstrap_token,
                secure_cookies,
                config.oidc,
            ),
            tuner_count: config.tuner_count,
            runtime_versions: config.runtime_versions,
            started_at: Instant::now(),
            media: HttpTsSessionManager::new(reqwest::Client::new()),
            internal_reservations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            internal_token_hash: token_hash(&config.admin_bootstrap_token),
            source_repository,
            job_repository,
            catalog_repository,
            jellyfin_setup: Arc::new(RwLock::new(JellyfinSetup::available(
                &config.public_base_url,
                &config.output_token,
            ))),
            master_key: config.master_key,
        }
    }

    pub fn catalog(&self) -> Arc<RwLock<CatalogSnapshot>> {
        Arc::clone(&self.catalog)
    }

    /// Create or update the database output profile from environment configuration.
    ///
    /// On first creation the environment token hash is seeded. On subsequent
    /// calls the persisted current token hash is retained so a rotated token
    /// stays active across restarts. When the persisted current hash differs
    /// from the environment hash, the Jellyfin setup state becomes
    /// `regeneration-required` because the environment plaintext no longer
    /// matches the active token.
    ///
    /// # Errors
    ///
    /// Returns a persistence error if the profile cannot update.
    pub async fn initialize_output_profile(&self) -> Result<(), PersistenceError> {
        let Some(catalog) = &self.catalog_repository else {
            return Ok(());
        };
        catalog
            .ensure_environment_output_profile(
                OutputProfileTokenHash::from_sha256(self.output_token_hash),
                i32::from(self.tuner_count),
            )
            .await?;
        // Compare the environment token hash with the persisted current hash.
        // When they differ, a rotation occurred and the environment plaintext
        // is no longer the active token.
        let persisted_hash = catalog.environment_output_profile_token_hash().await?;
        let setup = match persisted_hash {
            Some(hash) if hash.as_bytes() == &self.output_token_hash => {
                JellyfinSetup::available(&self.public_base_url, &self.environment_output_token)
            }
            _ => JellyfinSetup::regeneration_required(),
        };
        *self.jellyfin_setup.write().await = setup;
        Ok(())
    }

    /// Load durable bootstrap bearer authorization state.
    ///
    /// # Errors
    ///
    /// Returns a persistence error if the state cannot load.
    pub async fn initialize_auth(&self) -> Result<(), PersistenceError> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        if !database.bootstrap_bearer_enabled().await? {
            self.auth.disable_bootstrap_bearer();
        }
        let records = database
            .list_operator_api_tokens()
            .await?
            .into_iter()
            .filter_map(|row| {
                let token_hash: [u8; 32] = row.token_hash.try_into().ok()?;
                row.revoked_at.is_none().then_some(OperatorTokenRecord {
                    id: row.id,
                    token_hash,
                    scopes: row.scopes,
                    expires_at: row.expires_at,
                })
            });
        self.auth.load_operator_tokens(records);
        Ok(())
    }

    /// Disable the bootstrap bearer after a successful administrator sign-in.
    ///
    /// The local password remains available as break-glass access.
    async fn disable_bootstrap_bearer(&self) -> Result<(), PersistenceError> {
        if let Some(database) = &self.database {
            database.disable_bootstrap_bearer().await?;
        }
        self.auth.disable_bootstrap_bearer();
        Ok(())
    }

    /// Replaces the effective provider stream for a channel generation.
    pub async fn set_stream_source(&self, source: ChannelStreamSource) {
        self.media.configure_provider(ProviderSpec::new(
            Arc::clone(&source.provider_pool_id),
            source.max_connections,
        ));
        self.catalog
            .write()
            .await
            .stream_sources
            .insert(source.channel_id, source);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub revision: u64,
    pub channels: Vec<ChannelRecord>,
    pub programmes: Vec<ProgrammeRecord>,
    #[serde(skip)]
    pub stream_sources: HashMap<Uuid, ChannelStreamSource>,
}

#[derive(Clone)]
pub struct ChannelStreamSource {
    pub channel_id: Uuid,
    pub provider_pool_id: Arc<str>,
    pub source_id: Arc<str>,
    pub generation: u64,
    pub upstream_url: Arc<str>,
    pub max_connections: usize,
    pub estimated_bitrate_bits_per_second: u64,
}

impl fmt::Debug for ChannelStreamSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelStreamSource")
            .field("channel_id", &self.channel_id)
            .field("provider_pool_id", &self.provider_pool_id)
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field("upstream_url", &"<redacted>")
            .field("max_connections", &self.max_connections)
            .field(
                "estimated_bitrate_bits_per_second",
                &self.estimated_bitrate_bits_per_second,
            )
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ChannelRecord {
    pub id: Uuid,
    pub number: String,
    pub name: String,
    pub group: Option<String>,
    pub logo_url: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgrammeRecord {
    pub channel_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub stops_at: DateTime<Utc>,
    pub title: String,
    pub description: Option<String>,
    pub categories: Vec<String>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct CreateChannelRequest {
    pub number: String,
    pub name: String,
    pub group: Option<String>,
    pub logo_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SystemInfo {
    channels: usize,
    healthy_streams: usize,
    active_sessions: usize,
    guide_coverage: f64,
    provider_connections: usize,
    provider_limit: usize,
    uptime_seconds: u64,
    versions: RuntimeVersions,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CreateSourceRequest {
    name: String,
    kind: String,
    endpoint: String,
    timezone: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateSourceRequest {
    max_connections: Option<i32>,
    timezone: Option<String>,
    enabled: Option<bool>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SourceResponse {
    id: String,
    name: String,
    kind: String,
    state: String,
    channels: usize,
    last_sync: DateTime<Utc>,
    endpoint: String,
    refresh_interval_seconds: i32,
    last_refreshed_at: Option<DateTime<Utc>>,
    max_connections: i32,
    timezone: String,
    enabled: bool,
}

impl From<&SourceSummary> for SourceResponse {
    fn from(source: &SourceSummary) -> Self {
        Self {
            id: source.id.to_string(),
            name: source.name.clone(),
            kind: source.kind.api_name().to_owned(),
            state: source.state.clone(),
            channels: source.channels,
            last_sync: source.last_sync,
            endpoint: source.endpoint.clone(),
            refresh_interval_seconds: source.refresh_interval_seconds,
            last_refreshed_at: source.last_refreshed_at,
            max_connections: source.max_connections,
            timezone: source.timezone.clone(),
            enabled: source.enabled,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct JobResponse {
    id: String,
    kind: String,
    status: String,
    progress: serde_json::Value,
    attempts: i32,
    max_attempts: i32,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl From<&JobRecord> for JobResponse {
    fn from(job: &JobRecord) -> Self {
        Self {
            id: job.id.to_string(),
            kind: job.kind.clone(),
            status: job.status.clone(),
            progress: redact_diagnostics(&job.progress),
            attempts: job.attempts,
            max_attempts: job.max_attempts,
            last_error: job.last_error.as_deref().map(redact_error),
            created_at: job.created_at,
            updated_at: job.updated_at,
            completed_at: job.completed_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ChannelResponse {
    id: String,
    number: String,
    name: String,
    group: String,
    tvg_id: String,
    streams: usize,
    primary_codec: String,
    bitrate_kbps: u64,
    state: String,
    enabled: bool,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ProgrammeResponse {
    id: String,
    channel: String,
    title: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    source: String,
    confidence: u8,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ChannelPageResponse {
    total: i64,
    limit: i64,
    offset: i64,
    items: Vec<ChannelResponse>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct GroupResponse {
    name: String,
    channel_count: i64,
    enabled_count: i64,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ProgrammePageResponse {
    total: i64,
    limit: i64,
    offset: i64,
    items: Vec<ProgrammeResponse>,
}

#[derive(Clone, Debug, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
struct PageQuery {
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    channel_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct DynamicEventResponse {
    id: String,
    group: String,
    raw_title: String,
    programme_title: String,
    channel_slot: String,
    start: DateTime<Utc>,
    state: String,
    template: String,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EventTemplateResponse {
    id: String,
    name: String,
    display_name: String,
    match_regex: String,
    channel_name_format: String,
    group_name: String,
    event_duration_hours: i32,
    past_date_grace_hours: i32,
    future_date_days: i32,
    timezone: String,
    filler_title: String,
    enabled: bool,
}

#[derive(Clone, Debug, serde::Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CreateEventTemplateRequest {
    name: String,
    display_name: String,
    match_regex: String,
    channel_name_format: String,
    group_name: String,
    #[serde(default = "default_duration")]
    event_duration_hours: i32,
    #[serde(default = "default_grace")]
    past_date_grace_hours: i32,
    #[serde(default = "default_future_days")]
    future_date_days: i32,
    #[serde(default = "default_event_timezone")]
    timezone: String,
    #[serde(default = "default_filler_title")]
    filler_title: String,
}

/// Partial update body for an event template. All fields are optional.
/// Omitted fields keep their stored value. `enabled` toggles the template.
#[derive(Clone, Debug, Default, serde::Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateEventTemplateRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    match_regex: Option<String>,
    #[serde(default)]
    channel_name_format: Option<String>,
    #[serde(default)]
    group_name: Option<String>,
    #[serde(default)]
    event_duration_hours: Option<i32>,
    #[serde(default)]
    past_date_grace_hours: Option<i32>,
    #[serde(default)]
    future_date_days: Option<i32>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    filler_title: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn default_duration() -> i32 {
    3
}
fn default_grace() -> i32 {
    4
}
fn default_future_days() -> i32 {
    2
}
fn default_event_timezone() -> String {
    "UTC".to_owned()
}
fn default_filler_title() -> String {
    "No programs available".to_owned()
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EventChannelResponse {
    id: String,
    template_id: String,
    channel_id: Option<String>,
    slot_number: i32,
    event_title: Option<String>,
    event_start: Option<DateTime<Utc>>,
    event_end: Option<DateTime<Utc>>,
    raw_stream_name: Option<String>,
    state: String,
}

#[derive(Clone, Debug, serde::Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
struct EventChannelQuery {
    #[serde(default)]
    template_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    provider_pool_id: String,
    source_id: String,
    configured_generation: u64,
    upstream_generation: u64,
    state: String,
    viewer_count: usize,
    retained_packets: usize,
    capacity_packets: usize,
    lag_events: u64,
    wrap_events: u64,
    overwritten_packets: u64,
    reconnect_attempts: u64,
    failover_attempts: u64,
    failure_count: u64,
    last_failure: Option<String>,
    provider_capacity: usize,
    provider_active_sessions: usize,
    provider_high_watermark: usize,
    provider_available_slots: usize,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JellyfinSetup {
    /// `available` when the published URLs are present. `regeneration-required`
    /// when a rotation occurred and the plaintext token is no longer retained.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xmltv_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdhr_device_url: Option<String>,
    pub guide_days_max: u16,
}

impl JellyfinSetup {
    /// Build the published Jellyfin setup URLs from the public base URL and the
    /// output token. The token stays embedded in the path and is never exposed
    /// as a separate field.
    pub fn available(public_base_url: &str, output_token: &str) -> Self {
        let base = public_base_url.trim_end_matches('/');
        Self {
            status: JELLYFIN_SETUP_AVAILABLE.into(),
            playlist_url: Some(format!("{base}/out/{output_token}/playlist.m3u")),
            xmltv_url: Some(format!("{base}/out/{output_token}/xmltv.xml")),
            hdhr_device_url: Some(format!("{base}/out/{output_token}/hdhr/device.xml")),
            guide_days_max: GUIDE_DAYS_MAX,
        }
    }

    /// Build the response that states regeneration is required because a
    /// rotation occurred and the plaintext token is no longer retained.
    pub fn regeneration_required() -> Self {
        Self {
            status: JELLYFIN_SETUP_REGENERATION_REQUIRED.into(),
            playlist_url: None,
            xmltv_url: None,
            hdhr_device_url: None,
            guide_days_max: GUIDE_DAYS_MAX,
        }
    }
}

/// Maximum number of guide days that the backend accepts.
pub const GUIDE_DAYS_MAX: u16 = 30;

/// Setup status value when published URLs are present.
pub const JELLYFIN_SETUP_AVAILABLE: &str = "available";

/// Setup status value when a rotation occurred and regeneration is required.
pub const JELLYFIN_SETUP_REGENERATION_REQUIRED: &str = "regeneration-required";

/// Default overlap window in seconds for token rotation.
pub const TOKEN_ROTATION_OVERLAP_SECONDS: i64 = 300;

#[derive(Debug, Serialize, ToSchema)]
struct SaveResult {
    ok: bool,
    message: String,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SupportLogEntry {
    timestamp: DateTime<Utc>,
    level: String,
    message: String,
    context: serde_json::Value,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SupportBundleResponse {
    schema_version: String,
    generated_at: DateTime<Utc>,
    redaction: String,
    system: SystemInfo,
    jobs: Vec<JobResponse>,
    sessions: Vec<SessionResponse>,
    stream_health: StreamHealthStatsResponse,
    logs: Vec<SupportLogEntry>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(rename_all = "snake_case")]
struct OidcCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct LogoutRequest {}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AuthUser {
    id: String,
    username: String,
    display_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<AuthUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oidc_enabled: Option<bool>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct OperatorApiTokenResponse {
    id: String,
    name: String,
    scopes: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    created_by: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct IssuedOperatorApiTokenResponse {
    #[serde(flatten)]
    metadata: OperatorApiTokenResponse,
    /// The plaintext token is returned only by create and rotate responses.
    token: String,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateOperatorApiTokenRequest {
    name: String,
    scopes: Vec<String>,
    #[serde(alias = "expires_at")]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RotateOperatorApiTokenRequest {
    name: Option<String>,
    scopes: Option<Vec<String>>,
    #[serde(alias = "expires_at")]
    expires_at: Option<OperatorTokenExpiration>,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(transparent)]
struct OperatorTokenExpiration(Option<DateTime<Utc>>);

#[derive(Debug, Serialize, ToSchema)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub code: String,
}

impl ProblemDetails {
    fn new(status: StatusCode, code: &str, title: &str, detail: impl Into<String>) -> Self {
        Self {
            type_uri: format!("https://iptv-gateway.invalid/problems/{code}"),
            title: title.to_owned(),
            status: status.as_u16(),
            detail: detail.into(),
            code: code.to_owned(),
        }
    }

    fn response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(self)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[derive(Debug, OpenApi)]
#[openapi(
    paths(auth_status, login, logout, oidc_start, oidc_callback, list_operator_api_tokens, create_operator_api_token, rotate_operator_api_token, revoke_operator_api_token, system_info, support_bundle, support_logs, settings_schema, get_effective_settings, list_operator_overrides, get_operator_scope_global, replace_operator_scope_global, list_operator_revisions_global, rollback_operator_scope_global, get_operator_scope_provider, replace_operator_scope_provider, list_operator_revisions_provider, rollback_operator_scope_provider, get_operator_scope_group, replace_operator_scope_group, list_operator_revisions_group, rollback_operator_scope_group, list_sources, create_source, delete_source, update_source, update_source_refresh_interval, trigger_source_sync, source_sync_status, cancel_source_sync, list_groups, list_jobs, cancel_job, list_channels, create_channel, channel_preview, channel_stream, set_channel_enabled, set_group_enabled, set_all_groups_enabled, list_programmes, reconcile_epg_mappings, list_reconciliation_revisions, rollback_reconciliation, list_epg_mappings, list_unmapped_channels, list_review_candidates, search_epg_channels, set_channel_epg_mapping, remove_channel_epg_mapping, resolve_review, list_events, list_event_templates, create_event_template, update_event_template, delete_event_template, list_event_channels, scan_event_channels, prune_event_channels, suggest_event_templates, list_sessions, session_events, catalog_events, jellyfin_setup, rotate_jellyfin_token, list_lineup_templates, create_lineup_template, delete_lineup_template, list_lineup_categories, list_lineup_template_channels, apply_lineup_template, list_stream_health, stream_health_stats, trigger_health_check, rank_all_streams, best_stream_for_channel, list_users, create_user, update_user, delete_user, list_channel_aliases, create_channel_alias, delete_channel_alias, resolve_channel_alias, list_recording_rules, create_recording_rule, delete_recording_rule, list_recordings, create_recording, delete_recording, recording_stats, list_stream_profiles, create_stream_profile, delete_stream_profile, assign_stream_profile, remove_stream_profile, get_region_settings, update_region_settings, apply_region_filter),
    components(schemas(LoginRequest, LogoutRequest, OidcCallbackQuery, AuthUser, AuthStatus, OperatorApiTokenResponse, IssuedOperatorApiTokenResponse, CreateOperatorApiTokenRequest, RotateOperatorApiTokenRequest, AuthUser, RuntimeVersions, SystemInfo, SupportLogEntry, SupportBundleResponse, SettingDefinition, EffectiveSetting, InheritanceSource, ApplyRequirement, EffectiveSettingsResponse, OperatorSettingScope, OperatorOverridesResponse, OperatorScopeResponse, ReplaceOperatorScopeRequest, OperatorRevisionResponse, RollbackOperatorScopeRequest, SourceResponse, CreateSourceRequest, UpdateSourceRequest, UpdateRefreshIntervalRequest, SourceSyncResponse, SourceSyncStatusResponse, GroupResponse, JobResponse, ChannelRecord, ChannelResponse, ChannelPageResponse, CreateChannelRequest, ProgrammeResponse, ProgrammePageResponse, PageQuery, DynamicEventResponse, EventTemplateResponse, CreateEventTemplateRequest, UpdateEventTemplateRequest, EventChannelResponse, EventTemplateSuggestionResponse, SessionResponse, JellyfinSetup, RotateJellyfinTokenRequest, SaveResult, ProblemDetails, LineupTemplateResponse, CreateLineupTemplateRequest, LineupCategoryResponse, LineupChannelResponse, LineupApplyStatsResponse, EpgMappingResponse, EpgMappingPageResponse, UnmappedChannelResponse, UnmappedChannelPageResponse, ReviewCandidateResponse, EpgChannelSearchResponse, EpgReconcileResponse, ReconciliationRevisionResponse, ReconciliationRollbackResponse, RollbackReconciliationRequest, SetEpgMappingRequest, ResolveReviewRequest, StreamHealthResponse, StreamHealthItem, StreamHealthStatsResponse, HealthCheckTriggerResponse, StreamRankResponse, BestStreamResponse, UserResponse, CreateUserRequest, UpdateUserRequest, ChannelAliasResponse, ChannelAliasPageResponse, CreateChannelAliasRequest, ResolveAliasResponse, RecordingRuleResponse, CreateRecordingRuleRequest, RecordingResponse, RecordingPageResponse, CreateRecordingRequest, RecordingStatsResponse, StreamProfileResponse, CreateStreamProfileRequest, AssignStreamProfileRequest, RegionSettingsResponse, RegionSettingsDto, RegionPrefixResponse, UpdateRegionSettingsRequest, ApplyRegionFilterRequest, RegionFilterResponse)),
    tags((name = "authentication"), (name = "system"), (name = "settings"), (name = "sources"), (name = "jobs"), (name = "channels"), (name = "guide"), (name = "sessions"), (name = "configuration"), (name = "lineups"), (name = "streams"), (name = "users"), (name = "aliases"), (name = "recordings"), (name = "stream-profiles"))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    let control = source_control_routes()
        .merge(guide_control_routes())
        .merge(event_control_routes())
        .merge(operations_control_routes())
        .merge(configuration_control_routes())
        .route("/api/v1/openapi.json", get(openapi));

    // SSE routes bypass the compression layer so events flush immediately.
    // CompressionLayer buffers the response body, which prevents SSE events
    // from flushing and causes the session-events endpoint to time out.
    let sse_routes = sse_control_routes();

    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route(
            "/internal/v1/provider-reservations",
            post(reserve_provider_slot),
        )
        .route(
            "/internal/v1/provider-reservations/{reservation_id}",
            axum::routing::delete(release_provider_slot),
        )
        .merge(control)
        .merge(output_routes())
        .layer(CompressionLayer::new())
        .merge(sse_routes)
        .layer(CatchPanicLayer::new())
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ProviderReservationRequest {
    pool_id: String,
    capacity: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProviderReservationResponse {
    reservation_id: String,
}

fn internal_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    token_hash(token) == state.internal_token_hash
}

async fn reserve_provider_slot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProviderReservationRequest>,
) -> Response {
    if !internal_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if request.pool_id.trim().is_empty() || request.capacity == 0 {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let reservation_id = Uuid::now_v7().to_string();
    let lease = match state.media.reserve_provider_slot(
        request.pool_id,
        request.capacity,
        format!("probe:{reservation_id}"),
    ) {
        Ok(lease) => lease,
        Err(error) => return provider_reservation_error_status(&error).into_response(),
    };
    let mut reservations = state
        .internal_reservations
        .lock()
        .expect("reservation mutex is not poisoned");
    let now = Instant::now();
    reservations.retain(|_, (_, expires_at)| *expires_at > now);
    reservations.insert(
        reservation_id.clone(),
        (lease, now + Duration::from_secs(30)),
    );
    Json(ProviderReservationResponse { reservation_id }).into_response()
}

fn provider_reservation_error_status(error: &AcquireError) -> StatusCode {
    match error {
        AcquireError::AtCapacity { .. } => StatusCode::CONFLICT,
        AcquireError::LeaseIdExhausted => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn release_provider_slot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(reservation_id): Path<String>,
) -> Response {
    if !internal_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let removed = state
        .internal_reservations
        .lock()
        .expect("reservation mutex is not poisoned")
        .remove(&reservation_id);
    if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn source_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/login", axum::routing::post(login))
        .route("/api/v1/auth/logout", axum::routing::post(logout))
        .route("/api/v1/auth/oidc/start", get(oidc_start))
        .route("/api/v1/auth/oidc/callback", get(oidc_callback))
        .route(
            "/api/v1/auth/tokens",
            get(list_operator_api_tokens).post(create_operator_api_token),
        )
        .route(
            "/api/v1/auth/tokens/{token_id}/rotate",
            post(rotate_operator_api_token),
        )
        .route(
            "/api/v1/auth/tokens/{token_id}/revoke",
            post(revoke_operator_api_token),
        )
        .route("/api/v1/system", get(system_info))
        .route("/api/v1/settings/schema", get(settings_schema))
        .route("/api/v1/settings/effective", get(get_effective_settings))
        .route("/api/v1/settings/overrides", get(list_operator_overrides))
        .route(
            "/api/v1/settings/overrides/global",
            get(get_operator_scope_global).put(replace_operator_scope_global),
        )
        .route(
            "/api/v1/settings/overrides/global/revisions",
            get(list_operator_revisions_global),
        )
        .route(
            "/api/v1/settings/overrides/global/rollback",
            axum::routing::post(rollback_operator_scope_global),
        )
        .route(
            "/api/v1/settings/overrides/providers/{provider_id}",
            get(get_operator_scope_provider).put(replace_operator_scope_provider),
        )
        .route(
            "/api/v1/settings/overrides/providers/{provider_id}/revisions",
            get(list_operator_revisions_provider),
        )
        .route(
            "/api/v1/settings/overrides/providers/{provider_id}/rollback",
            axum::routing::post(rollback_operator_scope_provider),
        )
        .route(
            "/api/v1/settings/overrides/groups/{group_id}",
            get(get_operator_scope_group).put(replace_operator_scope_group),
        )
        .route(
            "/api/v1/settings/overrides/groups/{group_id}/revisions",
            get(list_operator_revisions_group),
        )
        .route(
            "/api/v1/settings/overrides/groups/{group_id}/rollback",
            axum::routing::post(rollback_operator_scope_group),
        )
        .route("/api/v1/sources", get(list_sources).post(create_source))
        .route(
            "/api/v1/sources/{source_id}",
            axum::routing::delete(delete_source).patch(update_source),
        )
        .route(
            "/api/v1/sources/{source_id}/refresh-interval",
            axum::routing::patch(update_source_refresh_interval),
        )
        .route(
            "/api/v1/sources/{source_id}/sync",
            axum::routing::post(trigger_source_sync),
        )
        .route(
            "/api/v1/sources/{source_id}/sync/cancel",
            axum::routing::post(cancel_source_sync),
        )
        .route(
            "/api/v1/sources/{source_id}/sync-status",
            get(source_sync_status),
        )
        .route("/api/v1/groups", get(list_groups))
        .route("/api/v1/jobs", get(list_jobs))
        .route(
            "/api/v1/jobs/{job_id}/cancel",
            axum::routing::post(cancel_job),
        )
}

fn guide_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/channels", get(list_channels).post(create_channel))
        .route(
            "/api/v1/channels/{channel_id}/preview",
            get(channel_preview),
        )
        .route("/api/v1/channels/{channel_id}/stream", get(channel_stream))
        .route(
            "/api/v1/channels/{channel_id}/enabled",
            axum::routing::patch(set_channel_enabled),
        )
        .route(
            "/api/v1/groups/{group_name}/enabled",
            axum::routing::patch(set_group_enabled),
        )
        .route(
            "/api/v1/groups/enabled",
            axum::routing::patch(set_all_groups_enabled),
        )
        .route("/api/v1/programmes", get(list_programmes))
        .route(
            "/api/v1/epg/reconcile",
            axum::routing::post(reconcile_epg_mappings),
        )
        .route(
            "/api/v1/sources/{source_id}/reconcile/revisions",
            get(list_reconciliation_revisions),
        )
        .route(
            "/api/v1/sources/{source_id}/reconcile/rollback",
            axum::routing::post(rollback_reconciliation),
        )
        .route("/api/v1/epg/mappings", get(list_epg_mappings))
        .route("/api/v1/epg/unmapped", get(list_unmapped_channels))
        .route(
            "/api/v1/epg/review/{channel_id}/candidates",
            get(list_review_candidates),
        )
        .route("/api/v1/epg/channels/search", get(search_epg_channels))
        .route(
            "/api/v1/channels/{channel_id}/epg-mapping",
            axum::routing::patch(set_channel_epg_mapping).delete(remove_channel_epg_mapping),
        )
        .route(
            "/api/v1/epg/review/{channel_id}/resolve",
            axum::routing::post(resolve_review),
        )
}

fn event_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/events", get(list_events))
        .route(
            "/api/v1/event-templates",
            get(list_event_templates).post(create_event_template),
        )
        .route(
            "/api/v1/event-templates/{template_id}",
            axum::routing::patch(update_event_template).delete(delete_event_template),
        )
        .route("/api/v1/event-channels", get(list_event_channels))
        .route(
            "/api/v1/event-templates/suggestions",
            get(suggest_event_templates),
        )
        .route(
            "/api/v1/event-templates/{template_id}/scan",
            axum::routing::post(scan_event_channels),
        )
        .route(
            "/api/v1/event-templates/{template_id}/prune",
            axum::routing::post(prune_event_channels),
        )
}

fn operations_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/sessions", get(list_sessions))
        .route("/api/v1/support/bundle", get(support_bundle))
        .route("/api/v1/support/logs", get(support_logs))
        .route("/api/v1/jellyfin/setup", get(jellyfin_setup))
        .route("/api/v1/jellyfin/setup/rotate", post(rotate_jellyfin_token))
        .route(
            "/api/v1/lineup-templates",
            get(list_lineup_templates).post(create_lineup_template),
        )
        .route(
            "/api/v1/lineup-templates/{template_id}",
            axum::routing::delete(delete_lineup_template),
        )
        .route(
            "/api/v1/lineup-templates/{template_id}/categories",
            get(list_lineup_categories),
        )
        .route(
            "/api/v1/lineup-templates/{template_id}/channels",
            get(list_lineup_template_channels),
        )
        .route(
            "/api/v1/lineup-templates/{template_id}/apply",
            axum::routing::post(apply_lineup_template),
        )
        .route("/api/v1/streams/health", get(list_stream_health))
        .route("/api/v1/streams/health/stats", get(stream_health_stats))
        .route(
            "/api/v1/streams/health/check",
            axum::routing::post(trigger_health_check),
        )
        .route(
            "/api/v1/streams/rank",
            axum::routing::post(rank_all_streams),
        )
        .route(
            "/api/v1/channels/{channel_id}/best-stream",
            get(best_stream_for_channel),
        )
}

/// SSE streaming routes that must bypass the compression layer.
///
/// `CompressionLayer` buffers the response body to inspect the
/// `Content-Type` header before it decides whether to compress. This
/// buffering prevents SSE events from flushing to the client and causes
/// the session-events endpoint to hang until the connection times out.
/// Keep these routes on a separate router that does not use compression.
fn sse_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/session-events", get(session_events))
        .route("/api/v1/catalog-events", get(catalog_events))
}

fn configuration_control_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/users", get(list_users).post(create_user))
        .route(
            "/api/v1/users/{user_id}",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route(
            "/api/v1/channel-aliases",
            get(list_channel_aliases).post(create_channel_alias),
        )
        .route(
            "/api/v1/channel-aliases/{alias_id}",
            axum::routing::delete(delete_channel_alias),
        )
        .route(
            "/api/v1/channel-aliases/resolve",
            get(resolve_channel_alias),
        )
        .route(
            "/api/v1/recordings/rules",
            get(list_recording_rules).post(create_recording_rule),
        )
        .route(
            "/api/v1/recordings/rules/{rule_id}",
            axum::routing::delete(delete_recording_rule),
        )
        .route(
            "/api/v1/recordings",
            get(list_recordings).post(create_recording),
        )
        .route(
            "/api/v1/recordings/{recording_id}",
            axum::routing::delete(delete_recording),
        )
        .route("/api/v1/recordings/stats", get(recording_stats))
        .route(
            "/api/v1/stream-profiles",
            get(list_stream_profiles).post(create_stream_profile),
        )
        .route(
            "/api/v1/stream-profiles/{profile_id}",
            axum::routing::delete(delete_stream_profile),
        )
        .route(
            "/api/v1/channels/{channel_id}/stream-profile",
            axum::routing::post(assign_stream_profile).delete(remove_stream_profile),
        )
        .route("/api/v1/region-settings", get(get_region_settings))
        .route(
            "/api/v1/region-settings",
            axum::routing::put(update_region_settings),
        )
        .route(
            "/api/v1/region-settings/apply",
            axum::routing::post(apply_region_filter),
        )
}

fn output_routes() -> Router<AppState> {
    Router::new()
        .route("/out/{token}/playlist.m3u", get(playlist))
        .route("/out/{token}/xmltv.xml", get(xmltv))
        .route("/out/{token}/stream/{*channel_path}", get(stream_channel))
        .route("/out/{token}/hdhr/discover.json", get(hdhr_discover))
        .route("/out/{token}/hdhr/lineup.json", get(hdhr_lineup))
        .route(
            "/out/{token}/hdhr/lineup_status.json",
            get(hdhr_lineup_status),
        )
        .route("/out/{token}/hdhr/device.xml", get(hdhr_device))
}

async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

async fn ready(State(state): State<AppState>) -> Response {
    if let Some(database) = &state.database
        && let Err(error) = database.health().await
    {
        return ProblemDetails::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "database-unavailable",
            "Database unavailable",
            error.to_string(),
        )
        .response();
    }
    (StatusCode::OK, Json(serde_json::json!({"status": "ready"}))).into_response()
}

async fn metrics(State(state): State<AppState>) -> Response {
    let sessions = state.media.list_snapshots();
    let mut pool_ids: HashSet<Arc<str>> = sessions
        .iter()
        .map(|snapshot| Arc::clone(&snapshot.key.provider_pool_id))
        .collect();
    {
        let catalog = state.catalog.read().await;
        pool_ids.extend(
            catalog
                .stream_sources
                .values()
                .map(|source| Arc::clone(&source.provider_pool_id)),
        );
    }
    let pools = pool_ids
        .into_iter()
        .filter_map(|pool_id| state.media.provider_snapshot(&pool_id))
        .collect::<Vec<_>>();
    text_response(
        "text/plain; version=0.0.4; charset=utf-8",
        prometheus_media_metrics(&sessions, &pools),
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MediaMetricValues {
    provider_pools: usize,
    provider_capacity: usize,
    provider_active_sessions: usize,
    provider_high_watermark: usize,
    provider_available_slots: usize,
    shared_sessions: usize,
    viewers: usize,
    retained_packets: usize,
    capacity_packets: usize,
    lag_events: u64,
    wrap_events: u64,
    overwritten_packets: u64,
    reconnect_attempts: u64,
    failover_attempts: u64,
    failures: u64,
}

fn media_metric_values(
    sessions: &[HttpTsSessionSnapshot],
    pools: &[PoolSnapshot],
) -> MediaMetricValues {
    let mut values = MediaMetricValues {
        provider_pools: pools.len(),
        shared_sessions: sessions.len(),
        ..MediaMetricValues::default()
    };
    for pool in pools {
        values.provider_capacity = values.provider_capacity.saturating_add(pool.capacity);
        values.provider_active_sessions = values
            .provider_active_sessions
            .saturating_add(pool.active_sessions);
        values.provider_high_watermark = values
            .provider_high_watermark
            .saturating_add(pool.high_watermark);
        values.provider_available_slots = values
            .provider_available_slots
            .saturating_add(pool.available_slots);
    }
    for session in sessions {
        values.viewers = values.viewers.saturating_add(session.viewer_count);
        values.retained_packets = values
            .retained_packets
            .saturating_add(session.ring.retained_packets);
        values.capacity_packets = values
            .capacity_packets
            .saturating_add(session.ring.capacity_packets);
        values.lag_events = values.lag_events.saturating_add(session.ring_lag_events);
        values.wrap_events = values.wrap_events.saturating_add(session.ring_wrap_events);
        values.overwritten_packets = values
            .overwritten_packets
            .saturating_add(session.overwritten_packets);
        values.reconnect_attempts = values
            .reconnect_attempts
            .saturating_add(session.reconnect_attempts);
        values.failover_attempts = values
            .failover_attempts
            .saturating_add(session.failover_attempts);
        values.failures = values.failures.saturating_add(session.failure_count);
    }
    values
}

fn prometheus_media_metrics(sessions: &[HttpTsSessionSnapshot], pools: &[PoolSnapshot]) -> String {
    let values = media_metric_values(sessions, pools);
    let mut output = String::with_capacity(2_048);
    write_prometheus_gauge(
        &mut output,
        "iptv_media_provider_pools",
        "Configured provider connection pools.",
        values.provider_pools,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_provider_capacity",
        "Total provider connection capacity.",
        values.provider_capacity,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_provider_active_sessions",
        "Provider slots currently occupied by shared sessions.",
        values.provider_active_sessions,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_provider_high_watermark",
        "Sum of per-pool process-lifetime high-water marks.",
        values.provider_high_watermark,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_provider_available_slots",
        "Provider connection slots currently available.",
        values.provider_available_slots,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_shared_sessions",
        "Live shared upstream sessions.",
        values.shared_sessions,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_viewers",
        "Downstream viewers attached to shared sessions.",
        values.viewers,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_ring_retained_packets",
        "MPEG-TS packets currently retained across live rings.",
        values.retained_packets,
    );
    write_prometheus_gauge(
        &mut output,
        "iptv_media_ring_capacity_packets",
        "MPEG-TS packet capacity across live rings.",
        values.capacity_packets,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_ring_lag_events_total",
        "Viewer lag events observed across shared sessions.",
        values.lag_events,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_ring_wrap_events_total",
        "Ring wrap events observed across shared sessions.",
        values.wrap_events,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_overwritten_packets_total",
        "Packets overwritten across shared session rings.",
        values.overwritten_packets,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_reconnect_attempts_total",
        "Upstream reconnect attempts across shared sessions.",
        values.reconnect_attempts,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_failover_attempts_total",
        "Fallback stream attempts across shared sessions.",
        values.failover_attempts,
    );
    write_prometheus_counter(
        &mut output,
        "iptv_media_failures_total",
        "Typed upstream failures across shared sessions.",
        values.failures,
    );
    output
}

fn write_prometheus_gauge(output: &mut String, name: &str, help: &str, value: impl fmt::Display) {
    writeln!(output, "# HELP {name} {help}").expect("writing to a String cannot fail");
    writeln!(output, "# TYPE {name} gauge").expect("writing to a String cannot fail");
    writeln!(output, "{name} {value}").expect("writing to a String cannot fail");
}

fn write_prometheus_counter(output: &mut String, name: &str, help: &str, value: impl fmt::Display) {
    writeln!(output, "# HELP {name} {help}").expect("writing to a String cannot fail");
    writeln!(output, "# TYPE {name} counter").expect("writing to a String cannot fail");
    writeln!(output, "{name} {value}").expect("writing to a String cannot fail");
}

#[utoipa::path(get, path = "/api/v1/auth/status", tag = "authentication", responses((status = 200, body = AuthStatus)))]
async fn auth_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let authenticated = state.auth.authorize(&headers, false).is_some();
    let mut response =
        Json(auth_status_body(authenticated, state.auth.oidc_enabled())).into_response();
    if let Some(cookie) = state.auth.ensure_csrf_cookie(&headers) {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("generated CSRF cookie is valid"),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/start",
    tag = "authentication",
    responses((status = 307), (status = 404, body = ProblemDetails), (status = 503, body = ProblemDetails))
)]
async fn oidc_start(State(state): State<AppState>) -> Response {
    match state.auth.start_oidc().await {
        Ok(authorization) => {
            let mut response = Redirect::temporary(&authorization.location).into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                HeaderValue::from_str(&authorization.state_cookie)
                    .expect("generated OIDC state cookie is valid"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => oidc_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/callback",
    tag = "authentication",
    params(OidcCallbackQuery),
    responses((status = 307), (status = 400, body = ProblemDetails), (status = 403, body = ProblemDetails), (status = 503, body = ProblemDetails))
)]
async fn oidc_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OidcCallbackQuery>,
) -> Response {
    let callback_state = query.state.as_deref().unwrap_or_default();
    let result = if query.error.is_some() {
        state
            .auth
            .cancel_oidc(&headers, callback_state)
            .map(|()| IssuedOidcResult::Rejected)
    } else {
        state
            .auth
            .complete_oidc_login(&headers, callback_state, query.code.as_deref())
            .await
            .map(IssuedOidcResult::Session)
    };

    let mut response = match result {
        Ok(IssuedOidcResult::Rejected) => oidc_error_response(OidcError::InvalidCallback),
        Ok(IssuedOidcResult::Session(session)) => {
            if state.disable_bootstrap_bearer().await.is_err() {
                ProblemDetails::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "authentication-unavailable",
                    "Authentication unavailable",
                    "the authentication service could not update the bootstrap authorization state",
                )
                .response()
            } else {
                let mut response =
                    Redirect::temporary(state.public_base_url.as_ref()).into_response();
                response.headers_mut().append(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&session.session_cookie)
                        .expect("generated session cookie is valid"),
                );
                response.headers_mut().append(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&session.csrf_cookie)
                        .expect("generated CSRF cookie is valid"),
                );
                response
            }
        }
        Err(error) => oidc_error_response(error),
    };
    let clear_cookie = state.auth.clear_oidc_state_cookie();
    if !clear_cookie.is_empty() {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&clear_cookie).expect("generated OIDC clearing cookie is valid"),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

enum IssuedOidcResult {
    Rejected,
    Session(crate::auth::IssuedSession),
}

fn oidc_error_response(error: OidcError) -> Response {
    let (status, code, title, detail) = match error {
        OidcError::NotConfigured => (
            StatusCode::NOT_FOUND,
            "oidc-not-configured",
            "OIDC is not configured",
            "configure an OIDC issuer, client, and allowlist before using this sign-in method",
        ),
        OidcError::InvalidConfiguration | OidcError::InvalidProvider => (
            StatusCode::SERVICE_UNAVAILABLE,
            "oidc-provider-invalid",
            "OIDC provider unavailable",
            "the configured OIDC provider metadata is invalid",
        ),
        OidcError::ProviderUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "oidc-provider-unavailable",
            "OIDC provider unavailable",
            "the OIDC provider did not complete the sign-in request",
        ),
        OidcError::InvalidState => (
            StatusCode::BAD_REQUEST,
            "oidc-invalid-state",
            "Invalid OIDC state",
            "the OIDC sign-in request expired or did not originate from this browser",
        ),
        OidcError::InvalidCallback => (
            StatusCode::BAD_REQUEST,
            "oidc-invalid-callback",
            "Invalid OIDC callback",
            "the OIDC provider returned an incomplete sign-in response",
        ),
        OidcError::IdentityNotAllowed => (
            StatusCode::FORBIDDEN,
            "oidc-identity-not-allowed",
            "OIDC identity not allowed",
            "the signed-in OIDC identity is not in the approved allowlist",
        ),
        OidcError::InvalidIdentityToken => (
            StatusCode::BAD_REQUEST,
            "oidc-invalid-identity-token",
            "Invalid OIDC identity token",
            "the OIDC identity token failed validation",
        ),
    };
    let mut response = ProblemDetails::new(status, code, title, detail).response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "authentication",
    request_body = LoginRequest,
    responses((status = 200, body = AuthStatus), (status = 400, body = ProblemDetails), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails), (status = 503, body = ProblemDetails))
)]
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<LoginRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return invalid_auth_request();
    };
    let verified = match state
        .auth
        .verify_login(&headers, request.username, request.password)
        .await
    {
        Ok(verified) => verified,
        Err(LoginError::CsrfValidationFailed) => {
            return ProblemDetails::new(
                StatusCode::FORBIDDEN,
                "csrf-validation-failed",
                "CSRF validation failed",
                "refresh the sign-in page and submit the CSRF token associated with this browser",
            )
            .response();
        }
        Err(LoginError::InvalidCredentials) => {
            return ProblemDetails::new(
                StatusCode::UNAUTHORIZED,
                "invalid-credentials",
                "Invalid credentials",
                "the username or password is incorrect",
            )
            .response();
        }
        Err(LoginError::Unavailable) => {
            return ProblemDetails::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication-unavailable",
                "Authentication unavailable",
                "the authentication service could not create a secure session",
            )
            .response();
        }
    };
    if state.disable_bootstrap_bearer().await.is_err() {
        return ProblemDetails::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "authentication-unavailable",
            "Authentication unavailable",
            "the authentication service could not update the bootstrap authorization state",
        )
        .response();
    }
    let session = match state.auth.complete_login(verified) {
        Ok(session) => session,
        Err(LoginError::Unavailable) => {
            return ProblemDetails::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication-unavailable",
                "Authentication unavailable",
                "the authentication service could not create a secure session",
            )
            .response();
        }
        Err(LoginError::CsrfValidationFailed | LoginError::InvalidCredentials) => {
            return ProblemDetails::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication-failed",
                "Authentication failed",
                "the authentication service could not complete the sign-in request",
            )
            .response();
        }
    };
    let mut response = Json(auth_status_body(true, state.auth.oidc_enabled())).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&session.session_cookie).expect("generated session cookie is valid"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&session.csrf_cookie).expect("generated CSRF cookie is valid"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[utoipa::path(post, path = "/api/v1/auth/logout", tag = "authentication", request_body = LogoutRequest, responses((status = 200, body = SaveResult), (status = 400, body = ProblemDetails), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails)))]
async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<LogoutRequest>, JsonRejection>,
) -> Response {
    if request.is_err() {
        return invalid_auth_request();
    }
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    state.auth.logout(&headers);
    let mut response = Json(SaveResult {
        ok: true,
        message: "Signed out.".to_owned(),
    })
    .into_response();
    for cookie in state.auth.clear_cookies() {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("generated clearing cookie is valid"),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/tokens",
    tag = "authentication",
    responses(
        (status = 200, description = "Operator API token metadata", body = Vec<OperatorApiTokenResponse>),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_operator_api_tokens(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_operator_token_admin(&state, &headers, false) {
        return response;
    }
    let Some(database) = &state.database else {
        return persistence_unavailable();
    };
    match database.list_operator_api_tokens().await {
        Ok(rows) => no_store_response(
            Json(
                rows.into_iter()
                    .map(OperatorApiTokenResponse::from)
                    .collect::<Vec<_>>(),
            )
            .into_response(),
        ),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/tokens",
    tag = "authentication",
    request_body = CreateOperatorApiTokenRequest,
    responses(
        (status = 201, description = "Operator API token created; plaintext is shown once", body = IssuedOperatorApiTokenResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn create_operator_api_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<CreateOperatorApiTokenRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_operator_token_admin(&state, &headers, true) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return invalid_operator_token_request();
    };
    let (name, scopes) = match validate_operator_token_input(&request.name, request.scopes) {
        Ok(values) => values,
        Err(detail) => return invalid_operator_token_detail(detail),
    };
    let Some(database) = &state.database else {
        return persistence_unavailable();
    };
    let Ok(plaintext) = AuthManager::generate_operator_token() else {
        return persistence_unavailable();
    };
    let token_hash = token_hash(&plaintext);
    let id = Uuid::now_v7();
    let actor = state.auth.operator_token_actor(&headers);
    let row = match database
        .create_operator_api_token(id, &name, &token_hash, &scopes, request.expires_at, &actor)
        .await
    {
        Ok(row) => row,
        Err(error) => return persistence_error_response(error),
    };
    state.auth.add_operator_token(OperatorTokenRecord {
        id,
        token_hash,
        scopes: row.scopes.clone(),
        expires_at: row.expires_at,
    });
    no_store_response(
        (
            StatusCode::CREATED,
            Json(IssuedOperatorApiTokenResponse {
                metadata: OperatorApiTokenResponse::from(row),
                token: plaintext,
            }),
        )
            .into_response(),
    )
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/tokens/{token_id}/rotate",
    tag = "authentication",
    params(("token_id" = Uuid, Path, description = "Token ID")),
    request_body = RotateOperatorApiTokenRequest,
    responses(
        (status = 200, description = "Operator API token rotated; plaintext is shown once", body = IssuedOperatorApiTokenResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rotate_operator_api_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_id): Path<Uuid>,
    request: Result<Json<RotateOperatorApiTokenRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_operator_token_admin(&state, &headers, true) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return invalid_operator_token_request();
    };
    let Some(database) = &state.database else {
        return persistence_unavailable();
    };
    let rows = match database.list_operator_api_tokens().await {
        Ok(rows) => rows,
        Err(error) => return persistence_error_response(error),
    };
    let Some(existing) = rows
        .into_iter()
        .find(|row| row.id == token_id && row.revoked_at.is_none())
    else {
        return operator_token_not_found();
    };
    let name = request.name.unwrap_or(existing.name);
    let scopes = request.scopes.unwrap_or(existing.scopes);
    let (name, scopes) = match validate_operator_token_input(&name, scopes) {
        Ok(values) => values,
        Err(detail) => return invalid_operator_token_detail(detail),
    };
    let expires_at = request
        .expires_at
        .map_or(existing.expires_at, |value| value.0);
    let Ok(plaintext) = AuthManager::generate_operator_token() else {
        return persistence_unavailable();
    };
    let token_hash = token_hash(&plaintext);
    let new_id = Uuid::now_v7();
    let actor = state.auth.operator_token_actor(&headers);
    let Some(row) = (match database
        .rotate_operator_api_token(
            token_id,
            new_id,
            &name,
            &token_hash,
            &scopes,
            expires_at,
            &actor,
        )
        .await
    {
        Ok(row) => row,
        Err(error) => return persistence_error_response(error),
    }) else {
        return operator_token_not_found();
    };
    state.auth.revoke_operator_token(token_id);
    state.auth.add_operator_token(OperatorTokenRecord {
        id: new_id,
        token_hash,
        scopes: row.scopes.clone(),
        expires_at: row.expires_at,
    });
    no_store_response(
        Json(IssuedOperatorApiTokenResponse {
            metadata: OperatorApiTokenResponse::from(row),
            token: plaintext,
        })
        .into_response(),
    )
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/tokens/{token_id}/revoke",
    tag = "authentication",
    params(("token_id" = Uuid, Path, description = "Token ID")),
    responses(
        (status = 204, description = "Operator API token revoked"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn revoke_operator_api_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_operator_token_admin(&state, &headers, true) {
        return response;
    }
    let Some(database) = &state.database else {
        return persistence_unavailable();
    };
    let actor = state.auth.operator_token_actor(&headers);
    match database.revoke_operator_api_token(token_id, &actor).await {
        Ok(true) => {
            state.auth.revoke_operator_token(token_id);
            no_store_response(StatusCode::NO_CONTENT.into_response())
        }
        Ok(false) => operator_token_not_found(),
        Err(error) => persistence_error_response(error),
    }
}

impl From<iptv_persistence::OperatorApiTokenRow> for OperatorApiTokenResponse {
    fn from(row: iptv_persistence::OperatorApiTokenRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name,
            scopes: row.scopes,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
            created_by: row.created_by,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        }
    }
}

fn validate_operator_token_input(
    name: &str,
    scopes: Vec<String>,
) -> Result<(String, Vec<String>), String> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("name must contain 1-128 non-control characters".to_owned());
    }
    let mut normalized = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let scope = scope.trim().to_ascii_lowercase();
        if !matches!(
            scope.as_str(),
            OPERATOR_TOKEN_READ_SCOPE
                | OPERATOR_TOKEN_CONTROL_SCOPE
                | OPERATOR_TOKEN_OUTPUT_SCOPE
                | OPERATOR_TOKEN_ADMIN_SCOPE
        ) {
            return Err("scopes must contain only read, control, output, or admin".to_owned());
        }
        if !normalized.contains(&scope) {
            normalized.push(scope);
        }
    }
    if normalized.is_empty() {
        return Err("at least one scope is required".to_owned());
    }
    Ok((name, normalized))
}

fn require_operator_token_admin(
    state: &AppState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Option<Response> {
    require_scope(state, headers, OPERATOR_TOKEN_ADMIN_SCOPE, require_csrf)
}

fn invalid_operator_token_request() -> Response {
    invalid_operator_token_detail(
        "send a JSON request that matches the documented operator token schema".to_owned(),
    )
}

fn invalid_operator_token_detail(detail: String) -> Response {
    ProblemDetails::new(
        StatusCode::BAD_REQUEST,
        "invalid-operator-api-token",
        "Invalid operator API token",
        detail,
    )
    .response()
}

fn operator_token_not_found() -> Response {
    ProblemDetails::new(
        StatusCode::NOT_FOUND,
        "operator-api-token-not-found",
        "Operator API token not found",
        "no active operator API token with that ID exists",
    )
    .response()
}

fn auth_status_body(authenticated: bool, oidc_enabled: bool) -> AuthStatus {
    AuthStatus {
        authenticated,
        user: authenticated.then(|| AuthUser {
            id: "operator-1".to_owned(),
            username: "operator".to_owned(),
            display_name: "Relay operator".to_owned(),
        }),
        oidc_enabled: oidc_enabled.then_some(true),
    }
}

fn invalid_auth_request() -> Response {
    ProblemDetails::new(
        StatusCode::BAD_REQUEST,
        "invalid-auth-request",
        "Invalid authentication request",
        "send a JSON request that matches the documented authentication schema",
    )
    .response()
}

fn persistence_unavailable() -> Response {
    ProblemDetails::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "persistence-unavailable",
        "Persistence unavailable",
        "the durable control-plane database is not available",
    )
    .response()
}

fn persistence_error_response(error: PersistenceError) -> Response {
    match error {
        PersistenceError::InvalidSource(detail) => ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-source",
            "Invalid source",
            detail,
        )
        .response(),
        PersistenceError::SourceConflict => ProblemDetails::new(
            StatusCode::CONFLICT,
            "source-conflict",
            "Source already exists",
            "a source with that name is already configured",
        )
        .response(),
        PersistenceError::SourceNotFound(source_id) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "source-not-found",
            "Source not found",
            format!("source {source_id} does not exist"),
        )
        .response(),
        PersistenceError::JobNotFound(job_id) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "job-not-found",
            "Job not found",
            format!("job {job_id} does not exist"),
        )
        .response(),
        PersistenceError::SettingRevisionConflict { expected, actual } => ProblemDetails::new(
            StatusCode::CONFLICT,
            "setting-revision-conflict",
            "Setting revision conflict",
            format!("expected revision {expected} but found {actual}"),
        )
        .response(),
        PersistenceError::SettingRevisionNotFound {
            scope,
            scope_id,
            target,
        } => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "setting-revision-not-found",
            "Setting revision not found",
            format!("revision {target} was not found for {scope}:{scope_id}"),
        )
        .response(),
        PersistenceError::ReconciliationRevisionNotFound { account_id, target } => {
            ProblemDetails::new(
                StatusCode::NOT_FOUND,
                "reconciliation-revision-not-found",
                "Reconciliation revision not found",
                format!("revision {target} was not found for account {account_id}"),
            )
            .response()
        }
        PersistenceError::InvalidMasterKey
        | PersistenceError::InvalidOutputProfileTunerCount
        | PersistenceError::Encryption
        | PersistenceError::Decryption
        | PersistenceError::Database(_)
        | PersistenceError::Migration(_)
        | PersistenceError::JobOwnership { .. } => persistence_unavailable(),
        PersistenceError::OperatorApiTokenNotFound(_) => operator_token_not_found(),
    }
}

#[utoipa::path(get, path = "/api/v1/system", tag = "system", responses((status = 200, body = SystemInfo)))]
#[allow(clippy::cast_precision_loss)]
async fn system_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    Json(system_info_value(&state).await).into_response()
}

#[allow(clippy::cast_precision_loss)]
async fn system_info_value(state: &AppState) -> SystemInfo {
    let mut pools = HashSet::new();
    let mut provider_connections = 0;
    let mut provider_limit = 0;
    {
        let catalog = state.catalog.read().await;
        for source in catalog.stream_sources.values() {
            if pools.insert(Arc::clone(&source.provider_pool_id))
                && let Some(snapshot) = state.media.provider_snapshot(&source.provider_pool_id)
            {
                provider_connections += snapshot.active_sessions;
                provider_limit += snapshot.capacity;
            }
        }
    }
    let (channels, healthy_streams, guide_coverage) =
        if let Some(catalog) = &state.catalog_repository {
            match catalog.system_counts().await {
                Ok(counts) => (
                    counts.channels,
                    counts.healthy_streams,
                    counts.guide_coverage,
                ),
                Err(_) => (0_i64, 0_i64, 0.0),
            }
        } else {
            let catalog = state.catalog.read().await;
            let channel_count = catalog
                .channels
                .iter()
                .filter(|channel| channel.enabled)
                .count();
            let guide_channels: HashSet<_> = catalog
                .programmes
                .iter()
                .map(|programme| programme.channel_id)
                .collect();
            let mapped = catalog
                .channels
                .iter()
                .filter(|channel| channel.enabled && guide_channels.contains(&channel.id))
                .count();
            let coverage = if channel_count == 0 {
                0.0
            } else {
                (mapped as f64 / channel_count as f64) * 100.0
            };
            (
                i64::try_from(channel_count).unwrap_or(i64::MAX),
                i64::try_from(catalog.stream_sources.len()).unwrap_or(i64::MAX),
                coverage,
            )
        };
    SystemInfo {
        channels: usize::try_from(channels).unwrap_or(usize::MAX),
        healthy_streams: usize::try_from(healthy_streams).unwrap_or(usize::MAX),
        active_sessions: provider_connections,
        guide_coverage,
        provider_connections,
        provider_limit,
        uptime_seconds: state.started_at.elapsed().as_secs(),
        versions: state.runtime_versions.clone(),
    }
}

fn support_log_from_audit(record: AuditEventRecord) -> SupportLogEntry {
    SupportLogEntry {
        timestamp: record.created_at,
        level: "info".to_owned(),
        message: record.action,
        context: redact_diagnostics(&serde_json::json!({
            "actor": record.actor,
            "resourceType": record.resource_type,
            "resourceId": record.resource_id,
            "correlationId": record.correlation_id,
            "details": record.details,
        })),
    }
}

fn support_log_from_job(job: &JobRecord) -> Option<SupportLogEntry> {
    job.last_error.as_ref().map(|error| SupportLogEntry {
        timestamp: job.updated_at,
        level: "error".to_owned(),
        message: redact_error(error),
        context: redact_diagnostics(&serde_json::json!({
            "jobId": job.id,
            "kind": job.kind,
            "status": job.status,
            "attempts": job.attempts,
            "progress": job.progress,
        })),
    })
}

async fn support_log_entries(state: &AppState) -> Result<Vec<SupportLogEntry>, PersistenceError> {
    let mut entries = Vec::new();
    if let Some(database) = &state.database {
        entries.extend(
            database
                .list_recent_audit_events(100)
                .await?
                .into_iter()
                .map(support_log_from_audit),
        );
    }
    if let Some(repository) = &state.job_repository {
        entries.extend(
            repository
                .list_recent(100)
                .await?
                .iter()
                .filter_map(support_log_from_job),
        );
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
    entries.truncate(100);
    Ok(entries)
}

#[utoipa::path(
    get,
    path = "/api/v1/support/bundle",
    tag = "system",
    responses(
        (status = 200, description = "A redacted support bundle", body = SupportBundleResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn support_bundle(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let jobs = match &state.job_repository {
        Some(repository) => match repository.list_recent(100).await {
            Ok(jobs) => jobs.iter().map(JobResponse::from).collect(),
            Err(error) => return persistence_error_response(error),
        },
        None => Vec::new(),
    };
    let logs = match support_log_entries(&state).await {
        Ok(logs) => logs,
        Err(error) => return persistence_error_response(error),
    };
    let stream_health = match &state.catalog_repository {
        Some(repository) => match repository.stream_health_stats().await {
            Ok(health_stats) => StreamHealthStatsResponse::from(health_stats),
            Err(error) => return persistence_error_response(error),
        },
        None => StreamHealthStatsResponse {
            alive: 0,
            dead: 0,
            unknown: 0,
            checking: 0,
        },
    };
    let bundle = SupportBundleResponse {
        schema_version: "1".to_owned(),
        generated_at: Utc::now(),
        redaction:
            "Secrets, tokens, credentials, URLs, and sensitive diagnostic fields are redacted."
                .to_owned(),
        system: system_info_value(&state).await,
        jobs,
        sessions: session_responses(&state.media),
        stream_health,
        logs,
    };
    // Redact the complete serialized document as a final defense against new
    // diagnostic fields that could contain credentials.
    let value =
        redact_diagnostics(&serde_json::to_value(bundle).expect("support bundle serializes"));
    Json(value).into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/support/logs",
    tag = "system",
    responses(
        (status = 200, description = "Recent redacted support log entries", body = [SupportLogEntry]),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn support_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match support_log_entries(&state).await {
        Ok(entries) => Json(redact_diagnostics(
            &serde_json::to_value(entries).expect("support logs serialize"),
        ))
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(get, path = "/api/v1/settings/schema", tag = "settings", responses((status = 200, body = [SettingDefinition]), (status = 401, body = ProblemDetails)))]
async fn settings_schema(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    Json(setting_catalog()).into_response()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum OperatorSettingScope {
    Global,
    Provider,
    Group,
}

impl OperatorSettingScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Provider => "provider",
            Self::Group => "group",
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EffectiveSettingsResponse {
    provider_id: String,
    group_id: String,
    settings: Vec<EffectiveSetting>,
    apply_requirements: Vec<ApplyRequirement>,
    etag: String,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct OperatorOverridesResponse {
    global: BTreeMap<String, serde_json::Value>,
    providers: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    groups: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct OperatorScopeResponse {
    scope: OperatorSettingScope,
    scope_id: String,
    overrides: BTreeMap<String, serde_json::Value>,
    revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ReplaceOperatorScopeRequest {
    overrides: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct OperatorRevisionResponse {
    revision: i64,
    actor: String,
    created_at: DateTime<Utc>,
    before_value: Option<serde_json::Value>,
    after_value: serde_json::Value,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RollbackOperatorScopeRequest {
    revision: i64,
}

#[allow(clippy::result_large_err)]
fn parse_if_match(headers: &HeaderMap) -> Result<Option<i64>, Response> {
    let Some(header_value) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    parse_if_match_value(header_value)
}

#[allow(clippy::result_large_err)]
fn parse_if_match_value(header_value: &HeaderValue) -> Result<Option<i64>, Response> {
    let text = header_value.to_str().map_err(|_| {
        ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-if-match",
            "Invalid If-Match header",
            "send an opaque ETag from the previous scope response",
        )
        .response()
    })?;
    let trimmed = text.trim().trim_start_matches("W/").trim_matches('"');
    let revision: i64 = trimmed.parse().map_err(|_| {
        ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-if-match",
            "Invalid If-Match header",
            "send an opaque ETag from the previous scope response",
        )
        .response()
    })?;
    Ok(Some(revision))
}

fn scope_etag(revision: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{revision}\"")).expect("valid ETag")
}

fn operator_scope_response(
    scope: OperatorSettingScope,
    scope_id: &str,
    state: OperatorSettingScopeState,
) -> Response {
    let mut response = Json(OperatorScopeResponse {
        scope,
        scope_id: scope_id.to_owned(),
        overrides: state.values,
        revision: state.revision,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::ETAG, scope_etag(state.revision));
    response
}

fn setting_validation_response(error: &SettingValidationError) -> Response {
    let status = match &error {
        SettingValidationError::UnknownSetting(_)
        | SettingValidationError::WrongType { .. }
        | SettingValidationError::OutOfRange { .. }
        | SettingValidationError::InvalidChoice { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        SettingValidationError::ProviderOverrideForbidden(_)
        | SettingValidationError::GroupOverrideForbidden(_) => StatusCode::FORBIDDEN,
    };
    ProblemDetails::new(
        status,
        "invalid-operator-setting",
        "Invalid operator setting",
        error.to_string(),
    )
    .response()
}

fn operator_persistence_error_response(error: PersistenceError) -> Response {
    match error {
        PersistenceError::SettingRevisionConflict { expected, actual } => {
            let mut response = ProblemDetails::new(
                StatusCode::PRECONDITION_FAILED,
                "setting-revision-conflict",
                "Setting revision conflict",
                format!(
                    "the stored revision is {actual}; the supplied If-Match expected {expected}. Refresh the scope and retry."
                ),
            )
            .response();
            response
                .headers_mut()
                .insert(header::ETAG, scope_etag(actual));
            response
        }
        PersistenceError::SettingRevisionNotFound {
            scope,
            scope_id,
            target,
        } => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "setting-revision-not-found",
            "Setting revision not found",
            format!("revision {target} does not exist for {scope}:{scope_id}"),
        )
        .response(),
        other => persistence_error_response(other),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/effective",
    tag = "settings",
    params(
        ("providerId" = Option<String>, Query, description = "Provider identifier used for provider-scope inheritance"),
        ("groupId" = Option<String>, Query, description = "Channel group identifier used for group-scope inheritance")
    ),
    responses(
        (status = 200, body = EffectiveSettingsResponse, description = "Effective settings with inheritance source and apply requirements"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn get_effective_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let provider_id = params.get("providerId").cloned().unwrap_or_default();
    let group_id = params.get("groupId").cloned().unwrap_or_default();
    let overrides = match catalog.load_operator_setting_overrides().await {
        Ok(overrides) => overrides,
        Err(error) => return persistence_error_response(error),
    };
    let domain_overrides = overrides.to_domain(&provider_id, &group_id);
    let settings = match resolve_settings(&domain_overrides, &provider_id, &group_id) {
        Ok(settings) => settings,
        Err(error) => return setting_validation_response(&error),
    };
    let apply_requirements = distinct_apply_requirements(&settings);
    let etag = effective_settings_etag(&settings);
    Json(EffectiveSettingsResponse {
        provider_id,
        group_id,
        settings,
        apply_requirements,
        etag,
    })
    .into_response()
}

fn distinct_apply_requirements(settings: &[EffectiveSetting]) -> Vec<ApplyRequirement> {
    let mut seen = Vec::new();
    for setting in settings {
        if !seen.contains(&setting.definition.apply_requirement) {
            seen.push(setting.definition.apply_requirement);
        }
    }
    seen
}

fn effective_settings_etag(settings: &[EffectiveSetting]) -> String {
    let mut digest = Sha256::new();
    for setting in settings {
        digest.update(setting.definition.key.as_bytes());
        digest.update(setting.value.to_string().as_bytes());
        digest.update(format!("{:?}", setting.inherited_from).as_bytes());
    }
    let hash = digest.finalize();
    hash.iter().take(8).fold(String::new(), |mut output, byte| {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
        output
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides",
    tag = "settings",
    responses(
        (status = 200, body = OperatorOverridesResponse, description = "All current operator overrides grouped by scope"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_operator_overrides(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.load_operator_setting_overrides().await {
        Ok(overrides) => Json(OperatorOverridesResponse {
            global: overrides.global,
            providers: overrides.providers,
            groups: overrides.groups,
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

async fn get_operator_scope(
    state: &AppState,
    headers: &HeaderMap,
    scope: OperatorSettingScope,
    scope_id: &str,
) -> Response {
    if let Some(response) = require_admin(state, headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .load_operator_setting_scope(scope.as_str(), scope_id)
        .await
    {
        Ok(stored) => operator_scope_response(scope, scope_id, stored),
        Err(error) => persistence_error_response(error),
    }
}

async fn replace_operator_scope(
    state: &AppState,
    headers: &HeaderMap,
    scope: OperatorSettingScope,
    scope_id: &str,
    request: Result<Json<ReplaceOperatorScopeRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(state, headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-operator-setting-request",
            "Invalid operator setting request",
            "send a JSON request with an overrides object",
        )
        .response();
    };
    let expected_revision = match parse_if_match(headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Err(error) = validate_scope_overrides(scope, &request.overrides) {
        return setting_validation_response(&error);
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .replace_operator_setting_scope(
            scope.as_str(),
            scope_id,
            &request.overrides,
            expected_revision,
            "operator",
        )
        .await
    {
        Ok(stored) => operator_scope_response(scope, scope_id, stored),
        Err(error) => operator_persistence_error_response(error),
    }
}

fn validate_scope_overrides(
    scope: OperatorSettingScope,
    overrides: &BTreeMap<String, serde_json::Value>,
) -> Result<(), SettingValidationError> {
    let catalog = setting_catalog();
    let known: BTreeMap<&str, &SettingDefinition> = catalog
        .iter()
        .map(|definition| (definition.key.as_str(), definition))
        .collect();
    for (key, value) in overrides {
        let Some(definition) = known.get(key.as_str()) else {
            return Err(SettingValidationError::UnknownSetting(key.clone()));
        };
        match scope {
            OperatorSettingScope::Provider if !definition.provider_overridable => {
                return Err(SettingValidationError::ProviderOverrideForbidden(
                    key.clone(),
                ));
            }
            OperatorSettingScope::Group if !definition.group_overridable => {
                return Err(SettingValidationError::GroupOverrideForbidden(key.clone()));
            }
            _ => {}
        }
        definition.validate(value)?;
    }
    Ok(())
}

async fn list_operator_revisions(
    state: &AppState,
    headers: &HeaderMap,
    scope: OperatorSettingScope,
    scope_id: &str,
) -> Response {
    if let Some(response) = require_admin(state, headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .list_operator_setting_revisions(
            scope.as_str(),
            scope_id,
            OPERATOR_SETTING_REVISION_PAGE_SIZE,
        )
        .await
    {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|row| OperatorRevisionResponse {
                    revision: row.revision,
                    actor: row.actor,
                    created_at: row.created_at,
                    before_value: row.before_value,
                    after_value: row.after_value,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

async fn rollback_operator_scope(
    state: &AppState,
    headers: &HeaderMap,
    scope: OperatorSettingScope,
    scope_id: &str,
    request: Result<Json<RollbackOperatorScopeRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(state, headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-operator-rollback-request",
            "Invalid rollback request",
            "send a JSON request with the target revision number",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .rollback_operator_setting_scope(scope.as_str(), scope_id, request.revision, "operator")
        .await
    {
        Ok(stored) => operator_scope_response(scope, scope_id, stored),
        Err(error) => operator_persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/global",
    tag = "settings",
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Current global overrides and revision"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn get_operator_scope_global(State(state): State<AppState>, headers: HeaderMap) -> Response {
    get_operator_scope(&state, &headers, OperatorSettingScope::Global, "").await
}

#[utoipa::path(
    put,
    path = "/api/v1/settings/overrides/global",
    tag = "settings",
    request_body = ReplaceOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Replaced global overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 412, body = ProblemDetails),
        (status = 422, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn replace_operator_scope_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ReplaceOperatorScopeRequest>, JsonRejection>,
) -> Response {
    replace_operator_scope(&state, &headers, OperatorSettingScope::Global, "", request).await
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/global/revisions",
    tag = "settings",
    responses(
        (status = 200, body = [OperatorRevisionResponse], description = "Global override revision history, newest first"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_operator_revisions_global(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    list_operator_revisions(&state, &headers, OperatorSettingScope::Global, "").await
}

#[utoipa::path(
    post,
    path = "/api/v1/settings/overrides/global/rollback",
    tag = "settings",
    request_body = RollbackOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Restored global overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rollback_operator_scope_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<RollbackOperatorScopeRequest>, JsonRejection>,
) -> Response {
    rollback_operator_scope(&state, &headers, OperatorSettingScope::Global, "", request).await
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/providers/{providerId}",
    tag = "settings",
    params(("providerId" = String, Path, description = "Provider identifier")),
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Current provider overrides and revision"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn get_operator_scope_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<String>,
) -> Response {
    get_operator_scope(
        &state,
        &headers,
        OperatorSettingScope::Provider,
        &provider_id,
    )
    .await
}

#[utoipa::path(
    put,
    path = "/api/v1/settings/overrides/providers/{providerId}",
    tag = "settings",
    params(("providerId" = String, Path, description = "Provider identifier")),
    request_body = ReplaceOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Replaced provider overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 412, body = ProblemDetails),
        (status = 422, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn replace_operator_scope_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<String>,
    request: Result<Json<ReplaceOperatorScopeRequest>, JsonRejection>,
) -> Response {
    replace_operator_scope(
        &state,
        &headers,
        OperatorSettingScope::Provider,
        &provider_id,
        request,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/providers/{providerId}/revisions",
    tag = "settings",
    params(("providerId" = String, Path, description = "Provider identifier")),
    responses(
        (status = 200, body = [OperatorRevisionResponse], description = "Provider override revision history, newest first"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_operator_revisions_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<String>,
) -> Response {
    list_operator_revisions(
        &state,
        &headers,
        OperatorSettingScope::Provider,
        &provider_id,
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/settings/overrides/providers/{providerId}/rollback",
    tag = "settings",
    params(("providerId" = String, Path, description = "Provider identifier")),
    request_body = RollbackOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Restored provider overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rollback_operator_scope_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<String>,
    request: Result<Json<RollbackOperatorScopeRequest>, JsonRejection>,
) -> Response {
    rollback_operator_scope(
        &state,
        &headers,
        OperatorSettingScope::Provider,
        &provider_id,
        request,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/groups/{groupId}",
    tag = "settings",
    params(("groupId" = String, Path, description = "Channel group identifier")),
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Current group overrides and revision"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn get_operator_scope_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Response {
    get_operator_scope(&state, &headers, OperatorSettingScope::Group, &group_id).await
}

#[utoipa::path(
    put,
    path = "/api/v1/settings/overrides/groups/{groupId}",
    tag = "settings",
    params(("groupId" = String, Path, description = "Channel group identifier")),
    request_body = ReplaceOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Replaced group overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 412, body = ProblemDetails),
        (status = 422, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn replace_operator_scope_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    request: Result<Json<ReplaceOperatorScopeRequest>, JsonRejection>,
) -> Response {
    replace_operator_scope(
        &state,
        &headers,
        OperatorSettingScope::Group,
        &group_id,
        request,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/api/v1/settings/overrides/groups/{groupId}/revisions",
    tag = "settings",
    params(("groupId" = String, Path, description = "Channel group identifier")),
    responses(
        (status = 200, body = [OperatorRevisionResponse], description = "Group override revision history, newest first"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_operator_revisions_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Response {
    list_operator_revisions(&state, &headers, OperatorSettingScope::Group, &group_id).await
}

#[utoipa::path(
    post,
    path = "/api/v1/settings/overrides/groups/{groupId}/rollback",
    tag = "settings",
    params(("groupId" = String, Path, description = "Channel group identifier")),
    request_body = RollbackOperatorScopeRequest,
    responses(
        (status = 200, body = OperatorScopeResponse, description = "Restored group overrides"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rollback_operator_scope_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    request: Result<Json<RollbackOperatorScopeRequest>, JsonRejection>,
) -> Response {
    rollback_operator_scope(
        &state,
        &headers,
        OperatorSettingScope::Group,
        &group_id,
        request,
    )
    .await
}

#[utoipa::path(get, path = "/api/v1/sources", tag = "sources", responses((status = 200, body = [SourceResponse])))]
async fn list_sources(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    match repository.list().await {
        Ok(sources) => {
            Json(sources.iter().map(SourceResponse::from).collect::<Vec<_>>()).into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/sources",
    tag = "sources",
    request_body = CreateSourceRequest,
    responses((status = 201, body = SourceResponse), (status = 409, body = ProblemDetails), (status = 422, body = ProblemDetails))
)]
async fn create_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<CreateSourceRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-request",
            "Invalid source request",
            "send a JSON request that matches the documented source schema",
        )
        .response();
    };
    let Some(repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    let kind = match SourceKind::parse_api(&request.kind) {
        Ok(kind) => kind,
        Err(error) => return persistence_error_response(error),
    };
    let source = NewSource {
        name: request.name,
        kind,
        endpoint: request.endpoint,
    };
    match repository
        .create_with_timezone(&source, "operator", request.timezone.as_deref())
        .await
    {
        Ok(created) => {
            // Enqueue an immediate refresh job so the first sync starts
            // without a manual trigger from the user.
            if let Some(job_repository) = &state.job_repository {
                let job = iptv_persistence::NewJob::immediate(
                    "refresh-source",
                    serde_json::json!({ "sourceId": created.source.id.to_string() }),
                );
                let _ = job_repository.enqueue(&job).await;
            }
            (
                StatusCode::CREATED,
                Json(SourceResponse::from(&created.source)),
            )
                .into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/sources/{source_id}",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source UUID")),
    responses((status = 204), (status = 404, body = ProblemDetails), (status = 401, body = ProblemDetails))
)]
async fn delete_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };
    match repository.delete(source_id, "operator").await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateRefreshIntervalRequest {
    refresh_interval_seconds: i32,
}

#[utoipa::path(
    patch,
    path = "/api/v1/sources/{source_id}/refresh-interval",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source ID")),
    request_body = UpdateRefreshIntervalRequest,
    responses(
        (status = 204, description = "Refresh interval updated"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails)
    )
)]
async fn update_source_refresh_interval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
    request: Result<Json<UpdateRefreshIntervalRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-refresh-interval-request",
            "Invalid refresh interval request",
            "send a JSON object with a refreshIntervalSeconds field",
        )
        .response();
    };
    if request.refresh_interval_seconds < 0 {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-refresh-interval",
            "Invalid refresh interval",
            "refresh interval must be zero or a positive number of seconds",
        )
        .response();
    }
    let Some(repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };
    match repository
        .update_refresh_interval(source_id, request.refresh_interval_seconds)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(PersistenceError::SourceNotFound(_)) => not_found(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SourceSyncResponse {
    job_id: String,
    message: String,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SourceSyncStatusResponse {
    job_id: String,
    status: String,
    stage: String,
    percent: u8,
    message: String,
    bytes_downloaded: u64,
    records_processed: u64,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SourceSyncStatusResponse {
    fn from_job(job: &JobRecord) -> Self {
        let progress = redact_diagnostics(&job.progress);
        let stage = progress
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let percent = progress
            .get("percent")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |value| u8::try_from(value).unwrap_or(u8::MAX));
        let message = progress
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let bytes_downloaded = progress
            .get("bytesDownloaded")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let records_processed = progress
            .get("recordsProcessed")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        Self {
            job_id: job.id.to_string(),
            status: job.status.clone(),
            stage,
            percent,
            message,
            bytes_downloaded,
            records_processed,
            started_at: job.created_at,
            updated_at: job.updated_at,
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/sources/{source_id}/sync",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source ID")),
    responses(
        (status = 202, body = SourceSyncResponse, description = "Refresh job enqueued"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 409, body = ProblemDetails, description = "A refresh job is already queued or running"),
        (status = 503, body = ProblemDetails, description = "Persistence layer unavailable")
    )
)]
async fn trigger_source_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(source_repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    let Some(job_repository) = &state.job_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };

    // Verify the source exists.
    let sources = match source_repository.list().await {
        Ok(sources) => sources,
        Err(error) => return persistence_error_response(error),
    };
    if !sources.iter().any(|s| s.id == source_id) {
        return not_found();
    }

    // Check for an existing queued or running refresh job for this source.
    let existing_jobs = match job_repository.list_recent(500).await {
        Ok(jobs) => jobs,
        Err(error) => return persistence_error_response(error),
    };
    let already_active = existing_jobs.iter().any(|job| {
        job.kind == "refresh-source"
            && job.payload.get("sourceId").and_then(|v| v.as_str()) == Some(&source_id.to_string())
            && (job.status == "queued" || job.status == "running")
    });
    if already_active {
        return ProblemDetails::new(
            StatusCode::CONFLICT,
            "refresh-already-active",
            "A refresh job is already queued or running",
            "wait for the current refresh to complete before you trigger a new one",
        )
        .response();
    }

    let job = iptv_persistence::NewJob::immediate(
        "refresh-source",
        serde_json::json!({ "sourceId": source_id.to_string() }),
    );
    match job_repository.enqueue(&job).await {
        Ok(record) => Json(SourceSyncResponse {
            job_id: record.id.to_string(),
            message: "Source refresh job enqueued.".to_owned(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/sources/{source_id}/sync-status",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source ID")),
    responses(
        (status = 200, body = SourceSyncStatusResponse, description = "Current sync progress for the source"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails, description = "No refresh job exists for the source"),
        (status = 503, body = ProblemDetails, description = "Persistence layer unavailable")
    )
)]
async fn source_sync_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(job_repository) = &state.job_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };
    let jobs = match job_repository.list_recent(500).await {
        Ok(jobs) => jobs,
        Err(error) => return persistence_error_response(error),
    };
    let target = source_id.to_string();
    let matches: Vec<_> = jobs
        .iter()
        .filter(|job| {
            job.kind == "refresh-source"
                && job.payload.get("sourceId").and_then(|v| v.as_str()) == Some(&target)
        })
        .collect();
    // Prefer an active job (running or queued) over a terminal one so the
    // UI shows live progress even when a retry or scheduled refresh created
    // a newer terminal job.
    let most_recent = matches
        .iter()
        .find(|job| job.status == "running" || job.status == "queued")
        .or_else(|| matches.first());
    match most_recent {
        Some(job) => Json(SourceSyncStatusResponse::from_job(job)).into_response(),
        None => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "sync-status-not-found",
            "Sync status not found",
            "no refresh job exists for the requested source",
        )
        .response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/sources/{source_id}/sync/cancel",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source ID")),
    responses(
        (status = 200, body = SaveResult, description = "Active sync cancelled"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails, description = "No active refresh job exists for the source"),
        (status = 409, body = ProblemDetails, description = "The refresh job is already in a terminal state"),
        (status = 503, body = ProblemDetails, description = "Persistence layer unavailable")
    )
)]
async fn cancel_source_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(job_repository) = &state.job_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };
    let jobs = match job_repository.list_recent(500).await {
        Ok(jobs) => jobs,
        Err(error) => return persistence_error_response(error),
    };
    let target = source_id.to_string();
    let active_job = jobs.iter().find(|job| {
        job.kind == "refresh-source"
            && job.payload.get("sourceId").and_then(|v| v.as_str()) == Some(&target)
            && (job.status == "queued" || job.status == "running")
    });
    let Some(job) = active_job else {
        return ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "sync-not-active",
            "No active sync to cancel",
            "no queued or running refresh job exists for the requested source",
        )
        .response();
    };
    match job_repository.cancel(job.id).await {
        Ok(true) => Json(SaveResult {
            ok: true,
            message: "Sync cancelled.".to_owned(),
        })
        .into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::CONFLICT,
            "sync-not-cancellable",
            "Sync cannot be cancelled",
            "the refresh job is already in a terminal state",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/sources/{source_id}",
    tag = "sources",
    params(("source_id" = String, Path, description = "Source ID")),
    request_body = UpdateSourceRequest,
    responses(
        (status = 204, description = "Source updated"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 422, body = ProblemDetails)
    )
)]
async fn update_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
    request: Result<Json<UpdateSourceRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-update-request",
            "Invalid source update request",
            "send a JSON object with optional maxConnections, timezone, or enabled fields",
        )
        .response();
    };
    if let Some(max_conn) = request.max_connections
        && max_conn < 1
    {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-max-connections",
            "Invalid max connections",
            "max connections must be one or greater",
        )
        .response();
    }
    let Some(repository) = &state.source_repository else {
        return persistence_unavailable();
    };
    let Ok(source_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source ID",
            "supply a valid UUID for the source ID",
        )
        .response();
    };
    let update = iptv_persistence::SourceUpdate {
        max_connections: request.max_connections,
        timezone: request.timezone,
        enabled: request.enabled,
    };
    match repository.update_source(source_id, &update).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(PersistenceError::SourceNotFound(_)) => not_found(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(get, path = "/api/v1/groups", tag = "channels", responses((status = 200, body = [GroupResponse])))]
async fn list_groups(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.list_groups().await {
        Ok(groups) => Json(
            groups
                .iter()
                .map(|g| GroupResponse {
                    name: g.name.clone(),
                    channel_count: g.channel_count,
                    enabled_count: g.enabled_count,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(get, path = "/api/v1/jobs", tag = "jobs", responses((status = 200, body = [JobResponse]), (status = 401, body = ProblemDetails), (status = 503, body = ProblemDetails)))]
async fn list_jobs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repository) = &state.job_repository else {
        return persistence_unavailable();
    };
    match repository.list_recent(100).await {
        Ok(jobs) => Json(jobs.iter().map(JobResponse::from).collect::<Vec<_>>()).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(post, path = "/api/v1/jobs/{job_id}/cancel", tag = "jobs", params(("job_id" = Uuid, Path, description = "Job identifier")), responses((status = 200, body = SaveResult), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails), (status = 404, body = ProblemDetails), (status = 409, body = ProblemDetails), (status = 503, body = ProblemDetails)))]
async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repository) = &state.job_repository else {
        return persistence_unavailable();
    };
    match repository.cancel(job_id).await {
        Ok(true) => Json(SaveResult {
            ok: true,
            message: "Cancellation requested.".to_owned(),
        })
        .into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::CONFLICT,
            "job-not-cancellable",
            "Job cannot be cancelled",
            "the job is already in a terminal state",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(get, path = "/api/v1/channels", tag = "channels", params(("search" = Option<String>, Query, description = "Case-insensitive name or group fragment"), ("group" = Option<String>, Query, description = "Exact group name"), ("enabled" = Option<bool>, Query, description = "Filter by enabled state"), ("limit" = Option<i64>, Query, description = "Page size, 1-500, default 100"), ("offset" = Option<i64>, Query, description = "Zero-based page offset")), responses((status = 200, body = ChannelPageResponse)))]
async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<PageQuery>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    if let Some(catalog) = &state.catalog_repository {
        let request = ChannelQuery {
            search: query.search,
            group: query.group,
            enabled: query.enabled,
            limit: query.limit,
            offset: query.offset,
        };
        return match catalog.list_channels(request).await {
            Ok(page) => Json(ChannelPageResponse {
                total: page.total,
                limit: page.limit,
                offset: page.offset,
                items: page.items.iter().map(channel_response_from_row).collect(),
            })
            .into_response(),
            Err(error) => persistence_error_response(error),
        };
    }
    let catalog = state.catalog.read().await;
    let items = catalog
        .channels
        .iter()
        .map(|channel| {
            let source = catalog.stream_sources.get(&channel.id);
            ChannelResponse {
                id: channel.id.to_string(),
                number: channel.number.clone(),
                name: channel.name.clone(),
                group: channel
                    .group
                    .clone()
                    .unwrap_or_else(|| "Uncategorized".to_owned()),
                tvg_id: channel.id.to_string(),
                streams: usize::from(source.is_some()),
                primary_codec: "Stream copy".to_owned(),
                bitrate_kbps: source
                    .map_or(0, |source| source.estimated_bitrate_bits_per_second / 1_000),
                state: if source.is_some() {
                    "healthy"
                } else {
                    "offline"
                }
                .to_owned(),
                enabled: channel.enabled,
            }
        })
        .collect::<Vec<_>>();
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Json(ChannelPageResponse {
        total,
        limit: query.limit.unwrap_or(100),
        offset: query.offset.unwrap_or(0),
        items,
    })
    .into_response()
}

fn channel_response_from_row(row: &iptv_persistence::ChannelRow) -> ChannelResponse {
    ChannelResponse {
        id: row.id.to_string(),
        number: row.channel_number.clone(),
        name: row.name.clone(),
        group: row.group_name.clone(),
        tvg_id: row.channel_number.clone(),
        streams: usize::try_from(row.stream_count).unwrap_or(usize::MAX),
        primary_codec: "Stream copy".to_owned(),
        bitrate_kbps: 0,
        state: if row.stream_count > 0 {
            "healthy"
        } else {
            "offline"
        }
        .to_owned(),
        enabled: row.enabled,
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ChannelPreviewResponse {
    stream_url: String,
    content_type: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/channels/{channel_id}/preview",
    tag = "channels",
    params(("channel_id" = String, Path, description = "Canonical channel UUID")),
    responses(
        (status = 200, body = ChannelPreviewResponse, description = "Preview URL for the admin-authenticated stream proxy"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn channel_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(uuid) = Uuid::parse_str(&channel_id).ok() else {
        return not_found();
    };
    let has_stream = has_channel_stream(&state, uuid).await;
    if !has_stream {
        return retryable_problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "stream-source-unavailable",
            "Stream source unavailable",
            "the channel has no active provider stream",
        );
    }
    let stream_url = format!("/api/v1/channels/{uuid}/stream");
    let mut response = Json(ChannelPreviewResponse {
        stream_url,
        content_type: "video/mp2t".to_owned(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// Checks whether a channel has an active stream source, consulting the
/// database when the in-memory catalog has no stream sources.
async fn has_channel_stream(state: &AppState, channel_id: Uuid) -> bool {
    {
        let catalog = state.catalog.read().await;
        if catalog.stream_sources.contains_key(&channel_id) {
            return true;
        }
    }
    if let Some(catalog) = &state.catalog_repository {
        return catalog
            .channel_playback_plan(channel_id)
            .await
            .is_ok_and(|plan| plan.is_some());
    }
    false
}

#[utoipa::path(
    get,
    path = "/api/v1/channels/{channel_id}/stream",
    tag = "channels",
    params(("channel_id" = String, Path, description = "Canonical channel UUID")),
    responses(
        (status = 200, description = "MPEG-TS byte stream", content_type = "video/mp2t"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
#[allow(clippy::too_many_lines)]
async fn channel_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(uuid) = Uuid::parse_str(&channel_id).ok() else {
        return not_found();
    };
    match open_channel_viewer(&state, uuid).await {
        Ok(viewer) => mpeg_ts_response(viewer),
        Err(response) => *response,
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/channels",
    tag = "channels",
    request_body = CreateChannelRequest,
    responses((status = 201, body = ChannelRecord), (status = 409, body = ProblemDetails))
)]
async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateChannelRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    if request.number.trim().is_empty() || request.name.trim().is_empty() {
        return ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-channel",
            "Invalid channel",
            "number and name are required",
        )
        .response();
    }

    let mut catalog = state.catalog.write().await;
    if catalog
        .channels
        .iter()
        .any(|channel| channel.number == request.number)
    {
        return ProblemDetails::new(
            StatusCode::CONFLICT,
            "channel-number-conflict",
            "Channel number already exists",
            format!("channel number {} is already assigned", request.number),
        )
        .response();
    }

    let channel = ChannelRecord {
        id: Uuid::now_v7(),
        number: request.number,
        name: request.name,
        group: request.group,
        logo_url: request.logo_url,
        enabled: true,
    };
    catalog.channels.push(channel.clone());
    catalog
        .channels
        .sort_by(|left, right| natural_number_cmp(&left.number, &right.number));
    catalog.revision += 1;

    let mut response = (StatusCode::CREATED, Json(channel)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", catalog.revision)).expect("valid ETag"),
    );
    response
}

#[derive(Clone, Debug, serde::Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SetEnabledRequest {
    enabled: bool,
}

#[utoipa::path(
    patch,
    path = "/api/v1/channels/{channel_id}/enabled",
    tag = "channels",
    params(("channel_id" = String, Path, description = "Canonical channel UUID")),
    request_body = SetEnabledRequest,
    responses(
        (status = 200, body = SaveResult, description = "Channel enabled state updated"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails)
    )
)]
async fn set_channel_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(uuid) = Uuid::parse_str(&channel_id).ok() else {
        return not_found();
    };
    if let Some(catalog) = &state.catalog_repository {
        match catalog.set_channel_enabled(uuid, request.enabled).await {
            Ok(0) => return not_found(),
            Ok(_) => {
                return Json(SaveResult {
                    ok: true,
                    message: "Channel updated".to_owned(),
                })
                .into_response();
            }
            Err(error) => return persistence_error_response(error),
        }
    }
    let mut catalog = state.catalog.write().await;
    if let Some(channel) = catalog
        .channels
        .iter_mut()
        .find(|channel| channel.id == uuid)
    {
        channel.enabled = request.enabled;
        catalog.revision += 1;
        Json(SaveResult {
            ok: true,
            message: "Channel updated".to_owned(),
        })
        .into_response()
    } else {
        not_found()
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/groups/{group_name}/enabled",
    tag = "channels",
    params(("group_name" = String, Path, description = "Channel group name")),
    request_body = SetEnabledRequest,
    responses(
        (status = 200, body = SaveResult, description = "Group enabled state updated"),
        (status = 401, body = ProblemDetails)
    )
)]
async fn set_group_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_name): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    if let Some(catalog) = &state.catalog_repository {
        match catalog
            .set_group_enabled(&group_name, request.enabled)
            .await
        {
            Ok(count) => {
                return Json(SaveResult {
                    ok: true,
                    message: format!("Updated {count} channels"),
                })
                .into_response();
            }
            Err(error) => return persistence_error_response(error),
        }
    }
    let mut catalog = state.catalog.write().await;
    let mut count = 0u64;
    for channel in &mut catalog.channels {
        if channel.group.as_deref() == Some(group_name.as_str()) {
            channel.enabled = request.enabled;
            count += 1;
        }
    }
    if count > 0 {
        catalog.revision += 1;
    }
    Json(SaveResult {
        ok: true,
        message: format!("Updated {count} channels"),
    })
    .into_response()
}

#[utoipa::path(
    patch,
    path = "/api/v1/groups/enabled",
    tag = "channels",
    request_body = SetEnabledRequest,
    responses(
        (status = 200, body = SaveResult, description = "All groups enabled state updated"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails)
    )
)]
async fn set_all_groups_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetEnabledRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    if let Some(catalog) = &state.catalog_repository {
        match catalog.set_all_groups_enabled(request.enabled).await {
            Ok(count) => {
                return Json(SaveResult {
                    ok: true,
                    message: format!("Updated {count} channels"),
                })
                .into_response();
            }
            Err(error) => return persistence_error_response(error),
        }
    }
    let mut catalog = state.catalog.write().await;
    let mut count = 0u64;
    for channel in &mut catalog.channels {
        if channel.enabled != request.enabled {
            channel.enabled = request.enabled;
            count += 1;
        }
    }
    if count > 0 {
        catalog.revision += 1;
    }
    Json(SaveResult {
        ok: true,
        message: format!("Updated {count} channels"),
    })
    .into_response()
}

#[utoipa::path(get, path = "/api/v1/programmes", tag = "guide", params(("channelId" = Option<Uuid>, Query, description = "Restrict to one canonical channel"), ("limit" = Option<i64>, Query, description = "Page size, 1-500, default 100"), ("offset" = Option<i64>, Query, description = "Zero-based page offset")), responses((status = 200, body = ProgrammePageResponse)))]
async fn list_programmes(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<PageQuery>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    if let Some(catalog) = &state.catalog_repository {
        let request = ProgrammeQuery {
            channel_id: query.channel_id,
            from: None,
            search: query.search,
            now: Some(Utc::now()),
            limit: query.limit,
            offset: query.offset,
        };
        return match catalog.list_programmes(request).await {
            Ok(page) => Json(ProgrammePageResponse {
                total: page.total,
                limit: page.limit,
                offset: page.offset,
                items: page.items.iter().map(programme_response_from_row).collect(),
            })
            .into_response(),
            Err(error) => persistence_error_response(error),
        };
    }
    let catalog = state.catalog.read().await;
    let channel_names: HashMap<_, _> = catalog
        .channels
        .iter()
        .map(|channel| (channel.id, channel.name.as_str()))
        .collect();
    let items = catalog
        .programmes
        .iter()
        .map(|programme| ProgrammeResponse {
            id: format!(
                "{}:{}",
                programme.channel_id,
                programme.starts_at.timestamp()
            ),
            channel: channel_names
                .get(&programme.channel_id)
                .copied()
                .unwrap_or("Unknown channel")
                .to_owned(),
            title: programme.title.clone(),
            start: programme.starts_at,
            end: programme.stops_at,
            source: "Imported guide".to_owned(),
            confidence: 100,
        })
        .collect::<Vec<_>>();
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Json(ProgrammePageResponse {
        total,
        limit: query.limit.unwrap_or(100),
        offset: query.offset.unwrap_or(0),
        items,
    })
    .into_response()
}

fn programme_response_from_row(row: &iptv_persistence::ProgrammeRow) -> ProgrammeResponse {
    ProgrammeResponse {
        id: row.id.clone(),
        channel: row.channel_name.clone(),
        title: row.title.clone(),
        start: row.starts_at,
        end: row.stops_at,
        source: row
            .source_name
            .clone()
            .unwrap_or_else(|| "Imported guide".to_owned()),
        confidence: 100,
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EpgMappingResponse {
    channel_id: String,
    epg_channel_id: String,
    method: String,
    confidence: f32,
    evidence: serde_json::Value,
    review_status: String,
    reviewed_by: Option<String>,
    reviewed_at: Option<DateTime<Utc>>,
    revision: i64,
    updated_at: DateTime<Utc>,
    channel_name: String,
    canonical_key: Option<String>,
    epg_xmltv_id: Option<String>,
    epg_display_name: Option<String>,
}

impl From<&iptv_persistence::EpgMappingRow> for EpgMappingResponse {
    fn from(row: &iptv_persistence::EpgMappingRow) -> Self {
        Self {
            channel_id: row.channel_id.to_string(),
            epg_channel_id: row.epg_channel_id.to_string(),
            method: row.method.clone(),
            confidence: row.confidence,
            evidence: row.evidence.clone(),
            review_status: row.review_status.clone(),
            reviewed_by: row.reviewed_by.clone(),
            reviewed_at: row.reviewed_at,
            revision: row.revision,
            updated_at: row.updated_at,
            channel_name: row.channel_name.clone(),
            canonical_key: row.canonical_key.clone(),
            epg_xmltv_id: row.epg_xmltv_id.clone(),
            epg_display_name: row.epg_display_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EpgMappingPageResponse {
    total: i64,
    limit: i64,
    offset: i64,
    items: Vec<EpgMappingResponse>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UnmappedChannelResponse {
    id: String,
    name: String,
    canonical_key: Option<String>,
    group_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UnmappedChannelPageResponse {
    total: i64,
    limit: i64,
    offset: i64,
    items: Vec<UnmappedChannelResponse>,
}

impl From<&iptv_persistence::UnmappedChannelRow> for UnmappedChannelResponse {
    fn from(row: &iptv_persistence::UnmappedChannelRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name.clone(),
            canonical_key: row.canonical_key.clone(),
            group_name: row.group_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ReviewCandidateResponse {
    id: String,
    channel_id: String,
    epg_channel_id: String,
    method: String,
    confidence: f32,
    evidence: serde_json::Value,
    created_at: DateTime<Utc>,
    epg_xmltv_id: Option<String>,
    epg_display_name: Option<String>,
}

impl From<&iptv_persistence::ReviewCandidateRow> for ReviewCandidateResponse {
    fn from(row: &iptv_persistence::ReviewCandidateRow) -> Self {
        Self {
            id: row.id.to_string(),
            channel_id: row.channel_id.to_string(),
            epg_channel_id: row.epg_channel_id.to_string(),
            method: row.method.clone(),
            confidence: row.confidence,
            evidence: row.evidence.clone(),
            created_at: row.created_at,
            epg_xmltv_id: row.epg_xmltv_id.clone(),
            epg_display_name: row.epg_display_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EpgChannelSearchResponse {
    id: String,
    xmltv_id: String,
    display_name: Option<String>,
}

impl From<&iptv_persistence::EpgChannelSearchRow> for EpgChannelSearchResponse {
    fn from(row: &iptv_persistence::EpgChannelSearchRow) -> Self {
        Self {
            id: row.id.to_string(),
            xmltv_id: row.xmltv_id.clone(),
            display_name: row.display_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EpgReconcileResponse {
    mappings_applied: i64,
    mappings_removed: i64,
    review_queued: i64,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ReconciliationRevisionResponse {
    revision: i64,
    actor: String,
    created_at: DateTime<Utc>,
    before_value: Option<serde_json::Value>,
    after_value: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ReconciliationRollbackResponse {
    target_revision: i64,
    channels_removed: i64,
    channels_restored: i64,
    stream_links_restored: i64,
    epg_mappings_restored: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RollbackReconciliationRequest {
    revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SetEpgMappingRequest {
    epg_channel_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ResolveReviewRequest {
    accept: bool,
    epg_channel_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/v1/epg/reconcile",
    tag = "guide",
    responses((status = 200, body = EpgReconcileResponse), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn reconcile_epg_mappings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.reconcile_epg_mappings().await {
        Ok(result) => Json(EpgReconcileResponse {
            mappings_applied: result.mappings_applied,
            mappings_removed: result.mappings_removed,
            review_queued: result.review_queued,
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/sources/{sourceId}/reconcile/revisions",
    tag = "guide",
    params(("sourceId" = String, Path, description = "Provider source identifier")),
    responses(
        (status = 200, body = [ReconciliationRevisionResponse], description = "Reconciliation revision history, newest first"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_reconciliation_revisions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(account_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source id",
            "send a valid provider account identifier",
        )
        .response();
    };
    match catalog.list_reconciliation_revisions(account_id).await {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|row| ReconciliationRevisionResponse {
                    revision: row.revision,
                    actor: row.actor,
                    created_at: row.created_at,
                    before_value: row.before_value,
                    after_value: row.after_value,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/sources/{sourceId}/reconcile/rollback",
    tag = "guide",
    params(("sourceId" = String, Path, description = "Provider source identifier")),
    request_body = RollbackReconciliationRequest,
    responses(
        (status = 200, body = ReconciliationRollbackResponse, description = "Restored channels, streams, and EPG mappings"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rollback_reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
    request: Result<Json<RollbackReconciliationRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-reconciliation-rollback-request",
            "Invalid rollback request",
            "send a JSON request with the target revision number",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(account_id) = Uuid::parse_str(&source_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-source-id",
            "Invalid source id",
            "send a valid provider account identifier",
        )
        .response();
    };
    match catalog
        .rollback_reconciliation(account_id, request.revision, "operator")
        .await
    {
        Ok(rollback_stats) => Json(ReconciliationRollbackResponse {
            target_revision: rollback_stats.target_revision,
            channels_removed: rollback_stats.channels_removed,
            channels_restored: rollback_stats.channels_restored,
            stream_links_restored: rollback_stats.stream_links_restored,
            epg_mappings_restored: rollback_stats.epg_mappings_restored,
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/epg/mappings",
    tag = "guide",
    params(
        ("reviewStatus" = Option<String>, Query, description = "Filter by review status: applied, review, rejected, manual"),
        ("limit" = Option<i64>, Query, description = "Page size, 1-500, default 100"),
        ("offset" = Option<i64>, Query, description = "Zero-based page offset")
    ),
    responses((status = 200, body = EpgMappingPageResponse), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn list_epg_mappings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let review_status = params.get("reviewStatus").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    match catalog
        .list_epg_mappings(review_status, limit, offset)
        .await
    {
        Ok(page) => Json(EpgMappingPageResponse {
            total: page.total,
            limit,
            offset,
            items: page.items.iter().map(EpgMappingResponse::from).collect(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/epg/unmapped",
    tag = "guide",
    params(
        ("search" = Option<String>, Query, description = "Search by channel name"),
        ("limit" = Option<i64>, Query, description = "Page size, 1-500, default 100"),
        ("offset" = Option<i64>, Query, description = "Zero-based page offset")
    ),
    responses((status = 200, body = UnmappedChannelPageResponse), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn list_unmapped_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let search = params.get("search").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    match catalog.list_unmapped_channels(search, limit, offset).await {
        Ok(page) => Json(UnmappedChannelPageResponse {
            total: page.total,
            limit,
            offset,
            items: page
                .items
                .iter()
                .map(UnmappedChannelResponse::from)
                .collect(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/epg/review/{channel_id}/candidates",
    tag = "guide",
    params(("channel_id" = String, Path, description = "Channel ID")),
    responses((status = 200, body = [ReviewCandidateResponse]), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn list_review_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(uuid) = Uuid::parse_str(&channel_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "supply a valid UUID for the channel ID",
        )
        .response();
    };
    match catalog.list_review_candidates(uuid).await {
        Ok(candidates) => Json(
            candidates
                .iter()
                .map(ReviewCandidateResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/epg/channels/search",
    tag = "guide",
    params(("q" = String, Query, description = "Search query"), ("limit" = Option<i64>, Query, description = "Max results, default 20")),
    responses((status = 200, body = [EpgChannelSearchResponse]), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn search_epg_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let query = params.get("q").map_or("", String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(20);
    match catalog.search_epg_channels(query, limit).await {
        Ok(rows) => Json(
            rows.iter()
                .map(EpgChannelSearchResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/channels/{channel_id}/epg-mapping",
    tag = "guide",
    params(("channel_id" = String, Path, description = "Channel ID")),
    request_body = SetEpgMappingRequest,
    responses(
        (status = 204, description = "Mapping set"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails)
    )
)]
async fn set_channel_epg_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    request: Result<Json<SetEpgMappingRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-mapping-request",
            "Invalid mapping request",
            "send a JSON object with an epgChannelId field",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(channel_uuid) = Uuid::parse_str(&channel_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "supply a valid UUID for the channel ID",
        )
        .response();
    };
    let Ok(epg_uuid) = Uuid::parse_str(&request.epg_channel_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-epg-channel-id",
            "Invalid EPG channel ID",
            "supply a valid UUID for the EPG channel ID",
        )
        .response();
    };
    let actor = "operator";
    match catalog
        .set_manual_epg_mapping(channel_uuid, epg_uuid, actor)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/channels/{channel_id}/epg-mapping",
    tag = "guide",
    params(("channel_id" = String, Path, description = "Channel ID")),
    responses(
        (status = 204, description = "Mapping removed"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails)
    )
)]
async fn remove_channel_epg_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(uuid) = Uuid::parse_str(&channel_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "supply a valid UUID for the channel ID",
        )
        .response();
    };
    match catalog.remove_epg_mapping(uuid).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/epg/review/{channel_id}/resolve",
    tag = "guide",
    params(("channel_id" = String, Path, description = "Channel ID")),
    request_body = ResolveReviewRequest,
    responses(
        (status = 204, description = "Review resolved"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails)
    )
)]
async fn resolve_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    request: Result<Json<ResolveReviewRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-resolve-request",
            "Invalid resolve request",
            "send a JSON object with an accept boolean and optional epgChannelId",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(channel_uuid) = Uuid::parse_str(&channel_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "supply a valid UUID for the channel ID",
        )
        .response();
    };
    let epg_uuid = request
        .epg_channel_id
        .as_deref()
        .map(|id| Uuid::parse_str(id).unwrap_or_else(|_| Uuid::nil()));
    let actor = "operator";
    match catalog
        .resolve_review(channel_uuid, request.accept, epg_uuid, actor)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(get, path = "/api/v1/events", tag = "guide", responses((status = 200, body = [DynamicEventResponse])))]
async fn list_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    if let Some(catalog) = &state.catalog_repository {
        match catalog.list_event_channels(None).await {
            Ok(channels) => {
                let templates = catalog
                    .list_event_templates(EventTemplateQuery::default())
                    .await;
                let template_names: HashMap<Uuid, String> = match templates {
                    Ok(rows) => rows.iter().map(|t| (t.id, t.name.clone())).collect(),
                    Err(_) => HashMap::new(),
                };
                let events: Vec<DynamicEventResponse> = channels
                    .iter()
                    .map(|c| dynamic_event_from_row(c, &template_names))
                    .collect();
                return Json(events).into_response();
            }
            Err(error) => return persistence_error_response(error),
        }
    }
    Json(Vec::<DynamicEventResponse>::new()).into_response()
}

fn dynamic_event_from_row(
    row: &iptv_persistence::EventChannelRow,
    template_names: &HashMap<Uuid, String>,
) -> DynamicEventResponse {
    DynamicEventResponse {
        id: row.id.to_string(),
        group: template_names
            .get(&row.template_id)
            .cloned()
            .unwrap_or_else(|| "Events".to_owned()),
        raw_title: row.raw_stream_name.clone().unwrap_or_default(),
        programme_title: row.event_title.clone().unwrap_or_default(),
        channel_slot: format!("{}", row.slot_number),
        start: row.event_start.unwrap_or_else(Utc::now),
        state: row.state.clone(),
        template: template_names
            .get(&row.template_id)
            .cloned()
            .unwrap_or_default(),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/event-templates",
    tag = "guide",
    responses((status = 200, body = [EventTemplateResponse]), (status = 401, body = ProblemDetails))
)]
async fn list_event_templates(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .list_event_templates(EventTemplateQuery::default())
        .await
    {
        Ok(templates) => Json(
            templates
                .iter()
                .map(event_template_response_from_row)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/event-templates",
    tag = "guide",
    request_body = CreateEventTemplateRequest,
    responses((status = 201, body = EventTemplateResponse), (status = 401, body = ProblemDetails), (status = 403, body = ProblemDetails))
)]
async fn create_event_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<CreateEventTemplateRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-event-template-request",
            "Invalid event template request",
            "send a JSON request that matches the documented event template schema",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let input = CreateEventTemplate {
        name: request.name,
        display_name: request.display_name,
        match_regex: request.match_regex,
        channel_name_format: request.channel_name_format,
        group_name: request.group_name,
        event_duration_hours: request.event_duration_hours,
        past_date_grace_hours: request.past_date_grace_hours,
        future_date_days: request.future_date_days,
        timezone: request.timezone,
        filler_title: request.filler_title,
    };
    match catalog.create_event_template(input).await {
        Ok(row) => (
            StatusCode::CREATED,
            Json(event_template_response_from_row(&row)),
        )
            .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/event-templates/{template_id}",
    tag = "guide",
    params(("template_id" = String, Path, description = "Event template UUID")),
    request_body = UpdateEventTemplateRequest,
    responses((status = 200, body = EventTemplateResponse), (status = 401, body = ProblemDetails), (status = 404, body = ProblemDetails))
)]
async fn update_event_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
    request: Result<Json<UpdateEventTemplateRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-event-template-request",
            "Invalid event template request",
            "send a JSON request that matches the documented event template patch schema",
        )
        .response();
    };
    let Ok(id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let input = EventTemplateUpdate {
        name: request.name,
        display_name: request.display_name,
        match_regex: request.match_regex,
        channel_name_format: request.channel_name_format,
        group_name: request.group_name,
        event_duration_hours: request.event_duration_hours,
        past_date_grace_hours: request.past_date_grace_hours,
        future_date_days: request.future_date_days,
        timezone: request.timezone,
        filler_title: request.filler_title,
        enabled: request.enabled,
    };
    match catalog.update_event_template_partial(id, &input).await {
        Ok(row) => Json(event_template_response_from_row(&row)).into_response(),
        Err(PersistenceError::SourceNotFound(_)) => not_found(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/event-templates/{template_id}",
    tag = "guide",
    params(("template_id" = String, Path, description = "Event template UUID")),
    responses((status = 204), (status = 401, body = ProblemDetails), (status = 404, body = ProblemDetails))
)]
async fn delete_event_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.delete_event_template(id).await {
        Ok(0) => not_found(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/event-channels",
    tag = "guide",
    params(("templateId" = Option<Uuid>, Query, description = "Filter by template ID")),
    responses((status = 200, body = [EventChannelResponse]), (status = 401, body = ProblemDetails))
)]
async fn list_event_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<EventChannelQuery>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.list_event_channels(query.template_id).await {
        Ok(channels) => Json(
            channels
                .iter()
                .map(event_channel_response_from_row)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/event-templates/{template_id}/scan",
    tag = "guide",
    params(("template_id" = String, Path, description = "Event template UUID")),
    responses((status = 200, body = SaveResult), (status = 401, body = ProblemDetails), (status = 404, body = ProblemDetails))
)]
async fn scan_event_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.scan_event_channels(id).await {
        Ok(count) => Json(SaveResult {
            ok: true,
            message: format!("Scanned {count} event channels"),
        })
        .into_response(),
        Err(PersistenceError::SourceNotFound(_)) => not_found(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/event-templates/{template_id}/prune",
    tag = "guide",
    params(("template_id" = String, Path, description = "Event template UUID")),
    responses((status = 200, body = SaveResult), (status = 401, body = ProblemDetails), (status = 404, body = ProblemDetails))
)]
async fn prune_event_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let templates = match catalog
        .list_event_templates(EventTemplateQuery::default())
        .await
    {
        Ok(rows) => rows,
        Err(error) => return persistence_error_response(error),
    };
    let grace = templates
        .iter()
        .find(|t| t.id == id)
        .map_or(4, |t| t.past_date_grace_hours);
    match catalog.prune_past_event_channels(grace).await {
        Ok(count) => Json(SaveResult {
            ok: true,
            message: format!("Pruned {count} event channels"),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct EventTemplateSuggestionResponse {
    name: String,
    display_name: String,
    match_regex: String,
    channel_name_format: String,
    group_name: String,
    event_duration_hours: i32,
    past_date_grace_hours: i32,
    future_date_days: i32,
    sample_streams: Vec<String>,
    stream_count: u64,
}

#[utoipa::path(
    get,
    path = "/api/v1/event-templates/suggestions",
    tag = "guide",
    responses(
        (status = 200, body = [EventTemplateSuggestionResponse], description = "Suggested event templates from live stream data"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn suggest_event_templates(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.suggest_event_templates().await {
        Ok(suggestions) => {
            let responses: Vec<EventTemplateSuggestionResponse> = suggestions
                .iter()
                .map(|s| EventTemplateSuggestionResponse {
                    name: s.name.clone(),
                    display_name: s.display_name.clone(),
                    match_regex: s.match_regex.clone(),
                    channel_name_format: s.channel_name_format.clone(),
                    group_name: s.group_name.clone(),
                    event_duration_hours: s.event_duration_hours,
                    past_date_grace_hours: s.past_date_grace_hours,
                    future_date_days: s.future_date_days,
                    sample_streams: s.sample_streams.clone(),
                    stream_count: s.stream_count,
                })
                .collect();
            Json(responses).into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

fn event_template_response_from_row(
    row: &iptv_persistence::EventTemplateRow,
) -> EventTemplateResponse {
    EventTemplateResponse {
        id: row.id.to_string(),
        name: row.name.clone(),
        display_name: row.display_name.clone(),
        match_regex: row.match_regex.clone(),
        channel_name_format: row.channel_name_format.clone(),
        group_name: row.group_name.clone(),
        event_duration_hours: row.event_duration_hours,
        past_date_grace_hours: row.past_date_grace_hours,
        future_date_days: row.future_date_days,
        timezone: row.timezone.clone(),
        filler_title: row.filler_title.clone(),
        enabled: row.enabled,
    }
}

fn event_channel_response_from_row(
    row: &iptv_persistence::EventChannelRow,
) -> EventChannelResponse {
    EventChannelResponse {
        id: row.id.to_string(),
        template_id: row.template_id.to_string(),
        channel_id: row.channel_id.map(|id| id.to_string()),
        slot_number: row.slot_number,
        event_title: row.event_title.clone(),
        event_start: row.event_start,
        event_end: row.event_end,
        raw_stream_name: row.raw_stream_name.clone(),
        state: row.state.clone(),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions",
    tag = "sessions",
    responses(
        (status = 200, body = [SessionResponse]),
        (status = 401, body = ProblemDetails)
    )
)]
async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    Json(session_responses(&state.media)).into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/session-events",
    tag = "sessions",
    responses(
        (status = 200, description = "A stream of named `sessions` events containing the same JSON array returned by `/api/v1/sessions`", content_type = "text/event-stream"),
        (status = 401, body = ProblemDetails)
    )
)]
async fn session_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let stream = async_stream::stream! {
        loop {
            let sessions = session_responses(&state.media);
            let data = serde_json::to_string(&sessions).expect("session snapshots serialize");
            yield Ok::<Event, Infallible>(Event::default().event("sessions").data(data));
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(5))
                .text("keepalive"),
        )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response.headers_mut().insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CatalogEvent {
    event: &'static str,
    timestamp: DateTime<Utc>,
}

/// Deduplicate refresh-source jobs by source ID for SSE emission.
///
/// Priority: running > queued > succeeded/failed > cancelled.
/// Terminal jobs are only included for 60 seconds after their last
/// update so the UI can clear stale progress bars.
fn dedup_sync_progress(jobs: &[JobRecord]) -> Vec<(&str, &JobRecord)> {
    let now = Utc::now();
    let priority = |status: &str| -> u8 {
        match status {
            "running" => 4,
            "queued" => 3,
            "succeeded" | "failed" => 2,
            "cancelled" => 1,
            _ => 0,
        }
    };
    let mut seen: std::collections::HashMap<&str, &JobRecord> = std::collections::HashMap::new();
    for job in jobs.iter().filter(|job| {
        if job.kind != "refresh-source" {
            return false;
        }
        match job.status.as_str() {
            "queued" | "running" => true,
            "succeeded" | "failed" | "cancelled" => (now - job.updated_at).num_seconds() < 60,
            _ => false,
        }
    }) {
        let source_id = job
            .payload
            .get("sourceId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let existing = seen.get(source_id);
        if existing.is_none()
            || priority(job.status.as_str()) > priority(existing.unwrap().status.as_str())
        {
            seen.insert(source_id, job);
        }
    }
    seen.into_iter().collect()
}

#[utoipa::path(
    get,
    path = "/api/v1/catalog-events",
    tag = "system",
    responses((status = 200, description = "Server-sent event stream of catalog changes"))
)]
async fn catalog_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let catalog_repository = state.catalog_repository.clone();
    let job_repository = state.job_repository.clone();
    let stream = async_stream::stream! {
        loop {
            if let Some(catalog) = &catalog_repository
                && let Ok(counts) = catalog.system_counts().await
            {
                let data = serde_json::to_string(&counts).expect("system counts serialize");
                yield Ok::<Event, Infallible>(Event::default().event("overview").data(data));
            }
            if let Some(jobs) = &job_repository
                && let Ok(recent) = jobs.list_recent(200).await
            {
                for (source_id, job) in dedup_sync_progress(&recent) {
                    let status = SourceSyncStatusResponse::from_job(job);
                    let payload = serde_json::json!({
                        "sourceId": source_id,
                        "jobId": status.job_id,
                        "status": status.status,
                        "stage": status.stage,
                        "percent": status.percent,
                        "message": status.message,
                        "bytesDownloaded": status.bytes_downloaded,
                        "recordsProcessed": status.records_processed,
                        "updatedAt": status.updated_at,
                    });
                    let data = serde_json::to_string(&payload).expect("sync progress serializes");
                    yield Ok::<Event, Infallible>(
                        Event::default()
                            .event("source-sync-progress")
                            .data(data),
                    );
                }
            }
            let event = serde_json::to_string(&CatalogEvent {
                event: "heartbeat",
                timestamp: Utc::now(),
            })
            .expect("catalog event serializes");
            yield Ok::<Event, Infallible>(Event::default().event("heartbeat").data(event));
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(5))
                .text("keepalive"),
        )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response.headers_mut().insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

fn session_responses(media: &HttpTsSessionManager) -> Vec<SessionResponse> {
    media
        .list_snapshots()
        .iter()
        .map(|snapshot| {
            let pool = media.provider_snapshot(&snapshot.key.provider_pool_id);
            session_response(snapshot, pool.as_ref())
        })
        .collect()
}

fn session_response(
    snapshot: &HttpTsSessionSnapshot,
    pool: Option<&PoolSnapshot>,
) -> SessionResponse {
    let (
        provider_capacity,
        provider_active_sessions,
        provider_high_watermark,
        provider_available_slots,
    ) = pool.map_or((0, 0, 0, 0), |pool| {
        (
            pool.capacity,
            pool.active_sessions,
            pool.high_watermark,
            pool.available_slots,
        )
    });
    SessionResponse {
        provider_pool_id: snapshot.key.provider_pool_id.to_string(),
        source_id: snapshot.key.source_id.to_string(),
        configured_generation: snapshot.key.generation,
        upstream_generation: snapshot.upstream_generation,
        state: session_state_name(snapshot.state).to_owned(),
        viewer_count: snapshot.viewer_count,
        retained_packets: snapshot.ring.retained_packets,
        capacity_packets: snapshot.ring.capacity_packets,
        lag_events: snapshot.ring_lag_events,
        wrap_events: snapshot.ring_wrap_events,
        overwritten_packets: snapshot.overwritten_packets,
        reconnect_attempts: snapshot.reconnect_attempts,
        failover_attempts: snapshot.failover_attempts,
        failure_count: snapshot.failure_count,
        last_failure: snapshot
            .last_failure
            .map(session_failure_name)
            .map(str::to_owned),
        provider_capacity,
        provider_active_sessions,
        provider_high_watermark,
        provider_available_slots,
    }
}

const fn session_state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Idle => "idle",
        SessionState::Reserving => "reserving",
        SessionState::Starting => "starting",
        SessionState::Priming => "priming",
        SessionState::Streaming => "streaming",
        SessionState::Recovering => "recovering",
        SessionState::FailingOver => "failing-over",
        SessionState::Stopping => "stopping",
        SessionState::Failed => "failed",
    }
}

const fn session_failure_name(failure: SessionFailureKind) -> &'static str {
    match failure {
        SessionFailureKind::UpstreamEnded => "upstream-ended",
        SessionFailureKind::Http => "http",
        SessionFailureKind::Packetization => "packetization",
        SessionFailureKind::Priming => "priming",
        SessionFailureKind::RecoveryExpired => "recovery-expired",
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/jellyfin/setup",
    tag = "configuration",
    responses((status = 200, body = JellyfinSetup), (status = 401, body = ProblemDetails))
)]
async fn jellyfin_setup(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_scope(&state, &headers, OPERATOR_TOKEN_OUTPUT_SCOPE, false) {
        return response;
    }
    let setup = state.jellyfin_setup.read().await.clone();
    let response = Json(setup).into_response();
    // The response may embed the output token in each URL path. Prevent
    // intermediary and browser caching so token-bearing URLs do not persist.
    no_store_response(response)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RotateJellyfinTokenRequest {
    #[serde(default = "default_rotation_overlap_seconds")]
    overlap_seconds: i64,
}

fn default_rotation_overlap_seconds() -> i64 {
    TOKEN_ROTATION_OVERLAP_SECONDS
}

#[utoipa::path(
    post,
    path = "/api/v1/jellyfin/setup/rotate",
    tag = "configuration",
    request_body = RotateJellyfinTokenRequest,
    responses(
        (status = 200, body = JellyfinSetup),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn rotate_jellyfin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<RotateJellyfinTokenRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_scope(&state, &headers, OPERATOR_TOKEN_OUTPUT_SCOPE, true) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-rotation-request",
            "Invalid rotation request",
            "send a JSON request that matches the documented rotation schema",
        )
        .response();
    };
    if !(0..=86_400).contains(&request.overlap_seconds) {
        return ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-overlap-window",
            "Invalid overlap window",
            "provide an overlap window between 0 and 86400 seconds",
        )
        .response();
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    // Generate a cryptographically random token. The plaintext is returned once
    // and is never stored in the database; only its SHA-256 hash is persisted.
    let Ok(plaintext_token) = generate_output_token() else {
        return ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "token-generation-failed",
            "Token generation failed",
            "the cryptographic random source could not produce a token",
        )
        .response();
    };
    let new_hash = OutputProfileTokenHash::from_sha256(token_hash(&plaintext_token));
    match catalog
        .rotate_environment_output_profile_token(new_hash, request.overlap_seconds)
        .await
    {
        Ok(_) => {
            let setup = JellyfinSetup::available(&state.public_base_url, &plaintext_token);
            // Mark future GET responses as regeneration-required because the
            // plaintext token is not retained after this response.
            *state.jellyfin_setup.write().await = JellyfinSetup::regeneration_required();
            let response = Json(setup).into_response();
            no_store_response(response)
        }
        Err(error) => persistence_error_response(error),
    }
}

/// Generate a cryptographically random output token encoded as URL-safe base64.
fn generate_output_token() -> Result<String, getrandom::Error> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

// Lineup template response and request types.

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupTemplateResponse {
    id: String,
    name: String,
    package_name: String,
    country: String,
    description: Option<String>,
    enabled: bool,
}

impl From<LineupTemplateRow> for LineupTemplateResponse {
    fn from(row: LineupTemplateRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name,
            package_name: row.package_name,
            country: row.country,
            description: row.description,
            enabled: row.enabled,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupCategoryResponse {
    id: String,
    template_id: String,
    name: String,
    sort_order: i32,
}

impl From<LineupCategoryRow> for LineupCategoryResponse {
    fn from(row: LineupCategoryRow) -> Self {
        Self {
            id: row.id.to_string(),
            template_id: row.template_id.to_string(),
            name: row.name,
            sort_order: row.sort_order,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupChannelResponse {
    id: String,
    category_id: String,
    name: String,
    channel_number: String,
    aliases: Vec<String>,
    enabled: bool,
}

impl From<LineupChannelRow> for LineupChannelResponse {
    fn from(row: LineupChannelRow) -> Self {
        Self {
            id: row.id.to_string(),
            category_id: row.category_id.to_string(),
            name: row.name,
            channel_number: row.channel_number,
            aliases: row.aliases,
            enabled: row.enabled,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupApplyStatsResponse {
    matched: u64,
    unmatched: u64,
    enabled: u64,
    disabled: u64,
}

impl From<LineupApplyStats> for LineupApplyStatsResponse {
    fn from(stats: LineupApplyStats) -> Self {
        Self {
            matched: stats.matched,
            unmatched: stats.unmatched,
            enabled: stats.enabled,
            disabled: stats.disabled,
        }
    }
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupChannelInput {
    name: String,
    channel_number: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct LineupCategoryInput {
    name: String,
    #[serde(default)]
    sort_order: i32,
    #[serde(default)]
    channels: Vec<LineupChannelInput>,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CreateLineupTemplateRequest {
    name: String,
    package_name: String,
    #[serde(default = "default_country")]
    country: String,
    description: Option<String>,
    #[serde(default)]
    categories: Vec<LineupCategoryInput>,
}

fn default_country() -> String {
    "US".to_owned()
}

#[utoipa::path(
    get,
    path = "/api/v1/lineup-templates",
    tag = "lineups",
    responses(
        (status = 200, body = [LineupTemplateResponse]),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_lineup_templates(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.list_lineup_templates().await {
        Ok(templates) => Json(
            templates
                .into_iter()
                .map(LineupTemplateResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/lineup-templates",
    tag = "lineups",
    request_body = CreateLineupTemplateRequest,
    responses(
        (status = 201, body = LineupTemplateResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn create_lineup_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<CreateLineupTemplateRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-lineup-template-request",
            "Invalid lineup template request",
            "send a JSON request that matches the documented lineup template schema",
        )
        .response();
    };
    if request.name.trim().is_empty() || request.package_name.trim().is_empty() {
        return ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-lineup-template",
            "Invalid lineup template",
            "name and package name are required",
        )
        .response();
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let template = match catalog
        .create_lineup_template(
            request.name.trim(),
            request.package_name.trim(),
            request.country.trim(),
            request.description.as_deref(),
        )
        .await
    {
        Ok(template) => template,
        Err(error) => return persistence_error_response(error),
    };
    if !request.categories.is_empty() {
        let categories_data = request
            .categories
            .into_iter()
            .map(|category| {
                let channels = category
                    .channels
                    .into_iter()
                    .map(|channel| (channel.name, channel.channel_number, channel.aliases))
                    .collect::<Vec<_>>();
                (category.name, category.sort_order, channels)
            })
            .collect::<Vec<_>>();
        if let Err(error) = catalog.import_lineup(template.id, categories_data).await {
            return persistence_error_response(error);
        }
    }
    (
        StatusCode::CREATED,
        Json(LineupTemplateResponse::from(template)),
    )
        .into_response()
}

#[utoipa::path(
    delete,
    path = "/api/v1/lineup-templates/{template_id}",
    tag = "lineups",
    params(("template_id" = String, Path, description = "Lineup template UUID")),
    responses(
        (status = 204),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn delete_lineup_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(template_id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    match catalog.delete_lineup_template(template_id).await {
        Ok(0) => not_found(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/lineup-templates/{template_id}/categories",
    tag = "lineups",
    params(("template_id" = String, Path, description = "Lineup template UUID")),
    responses(
        (status = 200, body = [LineupCategoryResponse]),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_lineup_categories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(template_id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    match catalog.list_lineup_categories(template_id).await {
        Ok(categories) => Json(
            categories
                .into_iter()
                .map(LineupCategoryResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/lineup-templates/{template_id}/channels",
    tag = "lineups",
    params(("template_id" = String, Path, description = "Lineup template UUID")),
    responses(
        (status = 200, body = [LineupChannelResponse]),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn list_lineup_template_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(template_id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    let categories = match catalog.list_lineup_categories(template_id).await {
        Ok(categories) => categories,
        Err(error) => return persistence_error_response(error),
    };
    let mut all_channels: Vec<LineupChannelResponse> = Vec::new();
    for category in categories {
        match catalog.list_lineup_channels(category.id).await {
            Ok(channels) => {
                all_channels.extend(channels.into_iter().map(LineupChannelResponse::from));
            }
            Err(error) => return persistence_error_response(error),
        }
    }
    Json(all_channels).into_response()
}

#[utoipa::path(
    post,
    path = "/api/v1/lineup-templates/{template_id}/apply",
    tag = "lineups",
    params(("template_id" = String, Path, description = "Lineup template UUID")),
    responses(
        (status = 200, body = LineupApplyStatsResponse),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn apply_lineup_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<String>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let Ok(template_id) = Uuid::parse_str(&template_id) else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-template-id",
            "Invalid template ID",
            "supply a valid UUID for the template ID",
        )
        .response();
    };
    match catalog.apply_lineup(template_id).await {
        Ok(apply_stats) => Json(LineupApplyStatsResponse::from(apply_stats)).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[derive(Clone, Debug)]
struct OutputAccess {
    profile: Option<OutputProfileRow>,
    tuner_count: u16,
}

async fn authorize_output(state: &AppState, token: &str) -> Result<OutputAccess, Response> {
    if let Some(catalog) = &state.catalog_repository {
        let token_hash = OutputProfileTokenHash::from_sha256(token_hash(token));
        let profile = catalog
            .resolve_enabled_output_profile(&token_hash)
            .await
            .map_err(persistence_error_response)?
            .ok_or_else(not_found)?;
        let tuner_count = u16::try_from(profile.tuner_count)
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(invalid_output_profile)?;
        return Ok(OutputAccess {
            profile: Some(profile),
            tuner_count,
        });
    }
    if !valid_output_token(state, token) {
        return Err(not_found());
    }
    Ok(OutputAccess {
        profile: None,
        tuner_count: state.tuner_count,
    })
}

async fn playlist(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let access = match authorize_output(&state, &token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    // Use the database catalog when available (production mode).
    if let Some(catalog_repo) = &state.catalog_repository {
        let profile = access
            .profile
            .as_ref()
            .expect("database output access has a profile");
        let channels = match catalog_repo
            .list_enabled_channels_for_output_profile(profile.id)
            .await
        {
            Ok(channels) => channels,
            Err(error) => return persistence_error_response(error),
        };
        let mut body = format!(
            "#EXTM3U x-tvg-url=\"{}/out/{}/xmltv.xml\"\n",
            state.public_base_url, token
        );
        for channel in channels {
            write!(
                &mut body,
                "#EXTINF:-1 tvg-id=\"{}\" tvg-name=\"{}\" tvg-logo=\"{}\" tvg-chno=\"{}\" group-title=\"{}\",{}\n{}/out/{}/stream/{}.ts\n",
                channel.id,
                m3u_escape(&channel.name),
                m3u_escape(channel.logo_url.as_deref().unwrap_or_default()),
                m3u_escape(&channel.channel_number),
                m3u_escape(&channel.group_name),
                channel.name.replace(['\r', '\n'], " "),
                state.public_base_url,
                token,
                channel.id,
            )
            .expect("writing to a String cannot fail");
        }
        return no_store_response(text_response(
            "application/vnd.apple.mpegurl; charset=utf-8",
            body,
        ));
    }
    let catalog = state.catalog.read().await;
    let mut body = format!(
        "#EXTM3U x-tvg-url=\"{}/out/{}/xmltv.xml\"\n",
        state.public_base_url, token
    );
    for channel in catalog.channels.iter().filter(|channel| channel.enabled) {
        let group = channel.group.as_deref().unwrap_or("Uncategorized");
        let logo = channel.logo_url.as_deref().unwrap_or_default();
        write!(
            &mut body,
            "#EXTINF:-1 tvg-id=\"{}\" tvg-name=\"{}\" tvg-logo=\"{}\" tvg-chno=\"{}\" group-title=\"{}\",{}\n{}/out/{}/stream/{}.ts\n",
            channel.id,
            m3u_escape(&channel.name),
            m3u_escape(logo),
            m3u_escape(&channel.number),
            m3u_escape(group),
            channel.name.replace(['\r', '\n'], " "),
            state.public_base_url,
            token,
            channel.id,
        )
        .expect("writing to a String cannot fail");
    }
    no_store_response(text_response(
        "application/vnd.apple.mpegurl; charset=utf-8",
        body,
    ))
}

#[allow(clippy::too_many_lines)]
async fn xmltv(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let access = match authorize_output(&state, &token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    // Use the database catalog when available (production mode).
    if let Some(catalog_repo) = &state.catalog_repository {
        let profile = access
            .profile
            .as_ref()
            .expect("database output access has a profile");
        let channels = match catalog_repo
            .list_enabled_channels_for_output_profile(profile.id)
            .await
        {
            Ok(channels) => channels,
            Err(error) => return persistence_error_response(error),
        };
        let channel_ids: Vec<Uuid> = channels.iter().map(|c| c.id).collect();
        let programmes = match catalog_repo
            .list_programmes_for_channels(&channel_ids)
            .await
        {
            Ok(programmes) => programmes,
            Err(error) => return persistence_error_response(error),
        };
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tv generator-info-name=\"IPTV Gateway\">\n",
        );
        let channel_by_id: HashMap<Uuid, &iptv_persistence::ChannelRow> =
            channels.iter().map(|c| (c.id, c)).collect();
        for channel in &channels {
            write!(
                &mut body,
                "  <channel id=\"{}\"><display-name>{}</display-name>",
                channel.id,
                xml_escape(&channel.name)
            )
            .expect("writing to a String cannot fail");
            if let Some(icon) = &channel.logo_url {
                write!(&mut body, "<icon src=\"{}\"/>", xml_escape(icon))
                    .expect("writing to a String cannot fail");
            }
            body.push_str("</channel>\n");
        }
        for programme in &programmes {
            // Resolve the channel UUID from the mapping row instead of a
            // name scan so the lookup stays constant time per programme.
            let Some(channel) = programme
                .channel_id
                .and_then(|id| channel_by_id.get(&id).copied())
                .or_else(|| channels.iter().find(|c| c.name == programme.channel_name))
            else {
                continue;
            };
            write!(
                &mut body,
                "  <programme channel=\"{}\" start=\"{}\" stop=\"{}\"><title>{}</title>",
                channel.id,
                programme.starts_at.format("%Y%m%d%H%M%S %z"),
                programme.stops_at.format("%Y%m%d%H%M%S %z"),
                xml_escape(&programme.title),
            )
            .expect("writing to a String cannot fail");
            if let Some(description) = &programme.description {
                write!(&mut body, "<desc>{}</desc>", xml_escape(description))
                    .expect("writing to a String cannot fail");
            }
            for category in programme.category_list() {
                write!(&mut body, "<category>{}</category>", xml_escape(&category))
                    .expect("writing to a String cannot fail");
            }
            body.push_str("</programme>\n");
        }
        body.push_str("</tv>\n");
        return no_store_response(text_response("application/xml; charset=utf-8", body));
    }
    let catalog = state.catalog.read().await;
    let enabled: HashSet<Uuid> = catalog
        .channels
        .iter()
        .filter(|channel| channel.enabled)
        .map(|channel| channel.id)
        .collect();
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tv generator-info-name=\"IPTV Gateway\">\n",
    );
    for channel in catalog.channels.iter().filter(|channel| channel.enabled) {
        write!(
            &mut body,
            "  <channel id=\"{}\"><display-name>{}</display-name>",
            channel.id,
            xml_escape(&channel.name)
        )
        .expect("writing to a String cannot fail");
        if let Some(icon) = &channel.logo_url {
            write!(&mut body, "<icon src=\"{}\"/>", xml_escape(icon))
                .expect("writing to a String cannot fail");
        }
        body.push_str("</channel>\n");
    }
    let mut programmes: Vec<_> = catalog
        .programmes
        .iter()
        .filter(|programme| enabled.contains(&programme.channel_id))
        .collect();
    programmes.sort_by_key(|programme| (programme.channel_id, programme.starts_at));
    for programme in programmes {
        write!(
            &mut body,
            "  <programme channel=\"{}\" start=\"{}\" stop=\"{}\"><title>{}</title>",
            programme.channel_id,
            programme.starts_at.format("%Y%m%d%H%M%S %z"),
            programme.stops_at.format("%Y%m%d%H%M%S %z"),
            xml_escape(&programme.title),
        )
        .expect("writing to a String cannot fail");
        if let Some(description) = &programme.description {
            write!(&mut body, "<desc>{}</desc>", xml_escape(description))
                .expect("writing to a String cannot fail");
        }
        for category in &programme.categories {
            write!(&mut body, "<category>{}</category>", xml_escape(category))
                .expect("writing to a String cannot fail");
        }
        body.push_str("</programme>\n");
    }
    body.push_str("</tv>\n");
    no_store_response(text_response("application/xml; charset=utf-8", body))
}

async fn stream_channel(
    State(state): State<AppState>,
    Path((token, channel_path)): Path<(String, String)>,
) -> Response {
    let access = match authorize_output(&state, &token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let Some(channel_id) = channel_path
        .strip_suffix(".ts")
        .and_then(|value| Uuid::parse_str(value).ok())
    else {
        return not_found();
    };
    if let (Some(catalog), Some(profile)) = (&state.catalog_repository, access.profile.as_ref()) {
        match catalog
            .output_profile_has_channel(profile.id, channel_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => return not_found(),
            Err(error) => return persistence_error_response(error),
        }
    }
    match open_channel_viewer(&state, channel_id).await {
        Ok(viewer) => mpeg_ts_response(viewer),
        Err(response) => *response,
    }
}

type StreamResult<T> = Result<T, Box<Response>>;

async fn open_channel_viewer(state: &AppState, channel_id: Uuid) -> StreamResult<ViewerHandle> {
    let spec = channel_source_spec(state, channel_id).await?;
    state
        .media
        .open(spec)
        .await
        .map_err(|error| Box::new(session_start_response(error)))
}

async fn channel_source_spec(state: &AppState, channel_id: Uuid) -> StreamResult<HttpTsSourceSpec> {
    let memory_source = {
        let catalog = state.catalog.read().await;
        let enabled = catalog
            .channels
            .iter()
            .any(|channel| channel.id == channel_id && channel.enabled);
        enabled
            .then(|| catalog.stream_sources.get(&channel_id).cloned())
            .flatten()
    };
    if let Some(source) = memory_source {
        return in_memory_source_spec(state, &source);
    }

    let Some(catalog) = &state.catalog_repository else {
        return Err(Box::new(stream_source_unavailable()));
    };
    let plan = catalog
        .channel_playback_plan(channel_id)
        .await
        .map_err(|error| Box::new(persistence_error_response(error)))?
        .ok_or_else(|| Box::new(stream_source_unavailable()))?;
    database_source_spec(state, &plan)
}

fn in_memory_source_spec(
    state: &AppState,
    source: &ChannelStreamSource,
) -> StreamResult<HttpTsSourceSpec> {
    state.media.configure_provider(ProviderSpec::new(
        Arc::clone(&source.provider_pool_id),
        source.max_connections,
    ));
    let ring = default_ring(source.estimated_bitrate_bits_per_second)
        .map_err(|detail| Box::new(invalid_buffer_policy(detail)))?;
    Ok(HttpTsSourceSpec::new(
        HttpTsSessionKey::new(
            Arc::clone(&source.provider_pool_id),
            Arc::clone(&source.source_id),
            source.generation,
        ),
        Arc::clone(&source.upstream_url),
        ring,
    ))
}

fn database_source_spec(
    state: &AppState,
    plan: &ChannelPlaybackPlan,
) -> StreamResult<HttpTsSourceSpec> {
    let Some(primary) = plan.candidates.first() else {
        return Err(Box::new(stream_source_unavailable()));
    };
    for candidate in &plan.candidates {
        let max_connections = usize::try_from(candidate.max_connections)
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| Box::new(invalid_provider_capacity()))?;
        state.media.configure_provider(ProviderSpec::new(
            candidate.provider_pool_id.to_string(),
            max_connections,
        ));
    }

    let bitrate = primary
        .bitrate_kbps
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| value.checked_mul(1_000))
        .unwrap_or(4_000_000);
    let ring = default_ring(bitrate).map_err(|detail| Box::new(invalid_buffer_policy(detail)))?;
    let primary_url = decrypt_stream_url(state, primary)?;
    let adapter_policy = input_adapter_policy(&primary.input_adapter)?;
    let primary_pool_id: Arc<str> = primary.provider_pool_id.to_string().into();
    let mut spec = HttpTsSourceSpec::new(
        HttpTsSessionKey::new(
            Arc::clone(&primary_pool_id),
            plan.channel_id.to_string(),
            playback_generation(plan),
        ),
        primary_url,
        ring,
    );
    spec.set_adapter_policy(adapter_policy);
    for candidate in plan.candidates.iter().skip(1) {
        spec.add_alternate(HttpTsEndpoint::for_provider(
            candidate.provider_pool_id.to_string(),
            decrypt_stream_url(state, candidate)?,
        ));
    }
    Ok(spec)
}

fn input_adapter_policy(value: &str) -> StreamResult<InputAdapterPolicy> {
    match value {
        "auto" => Ok(InputAdapterPolicy::Auto),
        "native-ts" => Ok(InputAdapterPolicy::NativeTs),
        "ffmpeg" => Ok(InputAdapterPolicy::Ffmpeg),
        "vlc" => Ok(InputAdapterPolicy::Vlc),
        _ => Err(Box::new(
            ProblemDetails::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid-input-adapter",
                "Invalid input adapter",
                "the persisted stream input adapter is not supported",
            )
            .response(),
        )),
    }
}

fn decrypt_stream_url(
    state: &AppState,
    candidate: &ChannelPlaybackCandidateRow,
) -> StreamResult<String> {
    let Some(ciphertext) = candidate.url_secret_ciphertext.as_deref() else {
        return Ok(candidate.stream_url.clone());
    };
    let associated_data = format!("iptv-provider-stream:v1:{}", candidate.provider_account_id);
    let plaintext = state
        .master_key
        .decrypt_secret(ciphertext, associated_data.as_bytes())
        .map_err(|_| Box::new(stream_secret_unavailable()))?;
    String::from_utf8(plaintext).map_err(|_| Box::new(stream_secret_unavailable()))
}

fn playback_generation(plan: &ChannelPlaybackPlan) -> u64 {
    let mut digest = Sha256::new();
    digest.update(plan.channel_id.as_bytes());
    digest.update(plan.channel_revision.to_be_bytes());
    let mut candidates: Vec<_> = plan.candidates.iter().collect();
    candidates.sort_by_key(|candidate| candidate.provider_stream_id);
    for candidate in candidates {
        digest.update(candidate.provider_stream_id.as_bytes());
        digest.update(candidate.source_snapshot_id.as_bytes());
        digest.update(candidate.provider_account_id.as_bytes());
        digest.update(candidate.provider_revision.to_be_bytes());
        digest.update(candidate.provider_pool_id.as_bytes());
        digest.update(candidate.input_adapter.as_bytes());
    }
    let output = digest.finalize();
    u64::from_be_bytes(
        output[..8]
            .try_into()
            .expect("SHA-256 output has eight bytes"),
    )
}

fn mpeg_ts_response(viewer: ViewerHandle) -> Response {
    let mut response = Response::new(Body::from_stream(viewer.into_byte_stream()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

fn session_start_response(error: SessionStartError) -> Response {
    match error {
        SessionStartError::Provider(AcquireError::AtCapacity { .. }) => retryable_problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider-capacity-exhausted",
            "Provider capacity exhausted",
            "all configured provider connections are in use",
        ),
        SessionStartError::Provider(AcquireError::LeaseIdExhausted) => ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider-lease-id-exhausted",
            "Provider lease allocation failed",
            "the media-session lease counter was exhausted",
        )
        .response(),
        SessionStartError::UnknownProvider { .. } => ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider-not-configured",
            "Provider is not configured",
            "the selected stream references a missing provider pool",
        )
        .response(),
        SessionStartError::StartupTimeout => retryable_problem(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream-startup-timeout",
            "Upstream startup timed out",
            "the provider did not return response headers before the configured timeout",
        ),
        SessionStartError::Http { message } => retryable_problem(
            StatusCode::BAD_GATEWAY,
            "upstream-start-failed",
            "Upstream stream failed to start",
            message.to_string(),
        ),
        SessionStartError::Process { adapter } => retryable_problem(
            StatusCode::BAD_GATEWAY,
            "upstream-adapter-failed",
            "Upstream adapter failed to start",
            format!("the {adapter:?} adapter could not start"),
        ),
        SessionStartError::ProcessInputRequiresProcessAdapter => ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "hls-process-adapter-required",
            "HLS adapter configuration required",
            "the HLS input requires the FFmpeg or VLC adapter",
        )
        .response(),
    }
}

fn stream_source_unavailable() -> Response {
    retryable_problem(
        StatusCode::SERVICE_UNAVAILABLE,
        "stream-source-unavailable",
        "Stream source unavailable",
        "the channel has no active provider stream",
    )
}

fn stream_secret_unavailable() -> Response {
    ProblemDetails::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "stream-secret-unavailable",
        "Stream secret unavailable",
        "the provider stream secret could not be decrypted",
    )
    .response()
}

fn invalid_provider_capacity() -> Response {
    ProblemDetails::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid-provider-capacity",
        "Invalid provider capacity",
        "the effective provider connection limit must be greater than zero",
    )
    .response()
}

fn invalid_output_profile() -> Response {
    ProblemDetails::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid-output-profile",
        "Invalid output profile",
        "the output profile tuner count is outside the supported range",
    )
    .response()
}

fn invalid_buffer_policy(detail: &'static str) -> Response {
    ProblemDetails::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid-buffer-policy",
        "Invalid buffer policy",
        detail,
    )
    .response()
}

fn default_ring(estimated_bitrate_bits_per_second: u64) -> Result<MpegTsRingConfig, &'static str> {
    const MAX_RING_BYTES: usize = 64 * 1024 * 1024;
    let maximum_packets = MAX_RING_BYTES / iptv_media::MPEG_TS_PACKET_SIZE;
    let maximum_bitrate = u64::try_from(maximum_packets)
        .map_err(|_| "the ring capacity cannot be represented on this platform")?
        .saturating_mul(iptv_media::MPEG_TS_PACKET_SIZE as u64);
    let bitrate = estimated_bitrate_bits_per_second.clamp(1, maximum_bitrate);
    let requested =
        MpegTsRingConfig::for_bitrate(bitrate, Duration::from_secs(8), Duration::from_secs(2))
            .map_err(|_| "the configured bitrate cannot be represented by the ring")?;
    MpegTsRingConfig::new(
        requested.capacity_packets().min(maximum_packets),
        requested.pre_roll_packets().min(maximum_packets),
    )
    .map_err(|_| "the configured pre-roll exceeds the effective ring capacity")
}

fn retryable_problem(
    status: StatusCode,
    code: &str,
    title: &str,
    detail: impl Into<String>,
) -> Response {
    let mut response = ProblemDetails::new(status, code, title, detail).response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct HdhrDiscover {
    friendly_name: &'static str,
    manufacturer: &'static str,
    model_number: &'static str,
    firmware_name: &'static str,
    firmware_version: String,
    device_id: &'static str,
    device_auth: String,
    base_url: String,
    lineup_url: String,
    tuner_count: u16,
}

async fn hdhr_discover(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let access = match authorize_output(&state, &token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let response = Json(HdhrDiscover {
        friendly_name: "IPTV Gateway",
        manufacturer: "IPTV Gateway",
        model_number: "HDHR-IPTV",
        firmware_name: "iptv-gateway",
        firmware_version: env!("CARGO_PKG_VERSION").to_owned(),
        device_id: "FFFFFFFF",
        device_auth: output_token_fingerprint(&token),
        base_url: format!("{}/out/{}/hdhr", state.public_base_url, token),
        lineup_url: format!("{}/out/{}/hdhr/lineup.json", state.public_base_url, token),
        tuner_count: access.tuner_count,
    })
    .into_response();
    no_store_response(response)
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct HdhrChannel {
    guide_number: String,
    guide_name: String,
    url: String,
}

async fn hdhr_lineup(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let access = match authorize_output(&state, &token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    if let Some(catalog) = &state.catalog_repository {
        let profile = access
            .profile
            .as_ref()
            .expect("database output access has a profile");
        let channels = match catalog
            .list_enabled_channels_for_output_profile(profile.id)
            .await
        {
            Ok(channels) => channels,
            Err(error) => return persistence_error_response(error),
        };
        let channels = channels
            .into_iter()
            .map(|channel| HdhrChannel {
                guide_number: channel.channel_number,
                guide_name: channel.name,
                url: format!(
                    "{}/out/{}/stream/{}.ts",
                    state.public_base_url, token, channel.id
                ),
            })
            .collect::<Vec<_>>();
        return no_store_response(Json(channels).into_response());
    }
    let catalog = state.catalog.read().await;
    let channels: Vec<_> = catalog
        .channels
        .iter()
        .filter(|channel| channel.enabled)
        .map(|channel| HdhrChannel {
            guide_number: channel.number.clone(),
            guide_name: channel.name.clone(),
            url: format!(
                "{}/out/{}/stream/{}.ts",
                state.public_base_url, token, channel.id
            ),
        })
        .collect();
    no_store_response(Json(channels).into_response())
}

async fn hdhr_lineup_status(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if let Err(response) = authorize_output(&state, &token).await {
        return response;
    }
    let response = Json(serde_json::json!({
        "ScanInProgress": 0,
        "ScanPossible": 1,
        "Source": "Cable",
        "SourceList": ["Cable"]
    }))
    .into_response();
    no_store_response(response)
}

async fn hdhr_device(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if let Err(response) = authorize_output(&state, &token).await {
        return response;
    }
    let xml = format!(
        "<?xml version=\"1.0\"?><root><device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType><friendlyName>IPTV Gateway</friendlyName><manufacturer>IPTV Gateway</manufacturer><modelName>HDHR-IPTV</modelName><UDN>uuid:{}</UDN></device></root>",
        Uuid::nil()
    );
    no_store_response(text_response("application/xml; charset=utf-8", xml))
}

#[derive(Debug, Serialize, ToSchema)]
struct StreamHealthItem {
    provider_stream_id: String,
    stream_name: String,
    group_name: Option<String>,
    health_status: String,
    health_checked_at: Option<String>,
    health_error: Option<String>,
    video_codec: Option<String>,
    video_resolution: Option<String>,
    video_width: Option<i32>,
    video_height: Option<i32>,
    video_fps: Option<f64>,
    audio_codec: Option<String>,
    audio_channels: Option<i32>,
    audio_sample_rate: Option<i32>,
    bitrate_kbps: Option<i32>,
    provider_account_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct StreamHealthResponse {
    total: i64,
    items: Vec<StreamHealthItem>,
    estimated: bool,
}

impl From<iptv_persistence::StreamHealthRow> for StreamHealthItem {
    fn from(row: iptv_persistence::StreamHealthRow) -> Self {
        Self {
            provider_stream_id: row.provider_stream_id.to_string(),
            stream_name: row.stream_name,
            group_name: row.group_name,
            health_status: row.health_status,
            health_checked_at: row.health_checked_at.map(|t| t.to_rfc3339()),
            health_error: row.health_error,
            video_codec: row.video_codec,
            video_resolution: row.video_resolution,
            video_width: row.video_width,
            video_height: row.video_height,
            video_fps: row.video_fps,
            audio_codec: row.audio_codec,
            audio_channels: row.audio_channels,
            audio_sample_rate: row.audio_sample_rate,
            bitrate_kbps: row.bitrate_kbps,
            provider_account_id: row.provider_account_id.to_string(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct StreamHealthStatsResponse {
    alive: i64,
    dead: i64,
    unknown: i64,
    checking: i64,
}

impl From<iptv_persistence::StreamHealthStats> for StreamHealthStatsResponse {
    fn from(stats: iptv_persistence::StreamHealthStats) -> Self {
        Self {
            alive: stats.alive,
            dead: stats.dead,
            unknown: stats.unknown,
            checking: stats.checking,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct BestStreamResponse {
    channel_id: String,
    channel_name: String,
    provider_stream_id: String,
    stream_name: String,
    priority: i32,
    quality_rank: i32,
    health_status: String,
    video_width: Option<i32>,
    video_height: Option<i32>,
    video_fps: Option<f64>,
    video_codec: Option<String>,
    failover_count: i32,
    last_failover_at: Option<String>,
    url_template: String,
    provider_account_id: String,
}

impl From<iptv_persistence::ChannelStreamCandidateRow> for BestStreamResponse {
    fn from(row: iptv_persistence::ChannelStreamCandidateRow) -> Self {
        Self {
            channel_id: row.channel_id.to_string(),
            channel_name: row.channel_name,
            provider_stream_id: row.provider_stream_id.to_string(),
            stream_name: row.stream_name,
            priority: row.priority,
            quality_rank: row.quality_rank,
            health_status: row.health_status,
            video_width: row.video_width,
            video_height: row.video_height,
            video_fps: row.video_fps,
            video_codec: row.video_codec,
            failover_count: row.failover_count,
            last_failover_at: row.last_failover_at.map(|t| t.to_rfc3339()),
            url_template: row.url_template,
            provider_account_id: row.provider_account_id.to_string(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/streams/health",
    tag = "streams",
    params(
        ("status" = Option<String>, Query, description = "Filter by health status: alive, dead, unknown, checking"),
        ("group" = Option<String>, Query, description = "Filter by group name"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
    ),
    responses(
        (status = 200, description = "Stream health data", body = StreamHealthResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_stream_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let status_filter = params.get("status").map(String::as_str);
    let group_filter = params.get("group").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    match repo
        .list_stream_health(status_filter, group_filter, limit, offset)
        .await
    {
        Ok(page) => Json(StreamHealthResponse {
            total: page.total,
            items: page.items.into_iter().map(StreamHealthItem::from).collect(),
            estimated: page.estimated,
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/streams/health/stats",
    tag = "streams",
    responses(
        (status = 200, description = "Stream health statistics", body = StreamHealthStatsResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn stream_health_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.stream_health_stats().await {
        Ok(result) => Json(StreamHealthStatsResponse::from(result)).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct TriggerHealthCheckRequest {
    limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
struct HealthCheckTriggerResponse {
    queued: i64,
}

#[utoipa::path(
    post,
    path = "/api/v1/streams/health/check",
    tag = "streams",
    request_body = TriggerHealthCheckRequest,
    responses(
        (status = 200, description = "Health check triggered", body = HealthCheckTriggerResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn trigger_health_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<TriggerHealthCheckRequest>>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let limit = body.and_then(|request| request.limit).unwrap_or(50);
    match repo.list_streams_needing_health_check(limit).await {
        Ok(streams) => {
            let queued = i64::try_from(streams.len()).unwrap_or(i64::MAX);
            // Mark streams as checking so they are not re-queued. A single
            // batch update replaces one round trip per stream.
            let ids: Vec<uuid::Uuid> = streams.iter().map(|s| s.provider_stream_id).collect();
            let _ = repo.mark_streams_checking(&ids).await;
            Json(HealthCheckTriggerResponse { queued }).into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct StreamRankResponse {
    ranked: i64,
}

#[utoipa::path(
    post,
    path = "/api/v1/streams/rank",
    tag = "streams",
    responses(
        (status = 200, description = "Streams ranked by quality", body = StreamRankResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn rank_all_streams(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.rank_all_channel_streams_by_quality().await {
        Ok(ranked) => Json(StreamRankResponse { ranked }).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/channels/{channel_id}/best-stream",
    tag = "streams",
    params(
        ("channel_id" = Uuid, Path, description = "Channel ID"),
    ),
    responses(
        (status = 200, description = "Best available stream for the channel", body = BestStreamResponse),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn best_stream_for_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.best_stream_for_channel(channel_id).await {
        Ok(Some(stream)) => Json(BestStreamResponse::from(stream)).into_response(),
        Ok(None) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "no-stream",
            "No stream available",
            "no provider stream is available for this channel",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct UserResponse {
    id: String,
    username: String,
    display_name: String,
    role: String,
    enabled: bool,
    last_login_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<iptv_persistence::UserRow> for UserResponse {
    fn from(row: iptv_persistence::UserRow) -> Self {
        Self {
            id: row.id.to_string(),
            username: row.username,
            display_name: row.display_name,
            role: row.role,
            enabled: row.enabled,
            last_login_at: row.last_login_at.map(|t| t.to_rfc3339()),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct CreateUserRequest {
    username: String,
    display_name: String,
    password: String,
    role: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct UpdateUserRequest {
    display_name: Option<String>,
    password: Option<String>,
    role: Option<String>,
    enabled: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/api/v1/users",
    tag = "users",
    responses(
        (status = 200, description = "List of user accounts", body = Vec<UserResponse>),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_users(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.list_users().await {
        Ok(users) => Json(
            users
                .into_iter()
                .map(UserResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/users",
    tag = "users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "User created", body = UserResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateUserRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let role = body.role.as_deref().unwrap_or("viewer");
    if !matches!(role, "admin" | "operator" | "viewer") {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-role",
            "Invalid role",
            "role must be admin, operator, or viewer",
        )
        .response();
    }
    let Ok(password_hash) = hash_admin_password(&body.password) else {
        return ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "hash-error",
            "Password hash failed",
            "could not hash the supplied password",
        )
        .response();
    };
    let input = iptv_persistence::CreateUserInput {
        username: body.username,
        display_name: body.display_name,
        password_hash,
        role: role.to_owned(),
    };
    match repo.create_user(&input).await {
        Ok(user) => (StatusCode::CREATED, Json(UserResponse::from(user))).into_response(),
        Err(PersistenceError::Database(error))
            if error.to_string().contains("unique constraint") =>
        {
            ProblemDetails::new(
                StatusCode::CONFLICT,
                "user-conflict",
                "User already exists",
                "a user with that username is already configured",
            )
            .response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/{user_id}",
    tag = "users",
    request_body = UpdateUserRequest,
    params(("user_id" = Uuid, Path, description = "User ID")),
    responses(
        (status = 200, description = "User updated", body = UserResponse),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(body): Json<UpdateUserRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    if let Some(ref role) = body.role
        && !matches!(role.as_str(), "admin" | "operator" | "viewer")
    {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-role",
            "Invalid role",
            "role must be admin, operator, or viewer",
        )
        .response();
    }
    let password_hash = if let Some(ref password) = body.password {
        match hash_admin_password(password) {
            Ok(hash) => Some(hash),
            Err(_) => {
                return ProblemDetails::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "hash-error",
                    "Password hash failed",
                    "could not hash the supplied password",
                )
                .response();
            }
        }
    } else {
        None
    };
    let input = iptv_persistence::UpdateUserInput {
        display_name: body.display_name,
        password_hash,
        role: body.role,
        enabled: body.enabled,
    };
    match repo.update_user(user_id, &input).await {
        Ok(Some(user)) => Json(UserResponse::from(user)).into_response(),
        Ok(None) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "user-not-found",
            "User not found",
            "no user with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/{user_id}",
    tag = "users",
    params(("user_id" = Uuid, Path, description = "User ID")),
    responses(
        (status = 204, description = "User deleted"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.delete_user(user_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "user-not-found",
            "User not found",
            "no user with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct ChannelAliasResponse {
    id: String,
    canonical_name: String,
    alias: String,
    country: Option<String>,
    category: Option<String>,
    created_at: String,
}

impl From<iptv_persistence::ChannelAliasRow> for ChannelAliasResponse {
    fn from(row: iptv_persistence::ChannelAliasRow) -> Self {
        Self {
            id: row.id.to_string(),
            canonical_name: row.canonical_name,
            alias: row.alias,
            country: row.country,
            category: row.category,
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct ChannelAliasPageResponse {
    total: i64,
    items: Vec<ChannelAliasResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct CreateChannelAliasRequest {
    canonical_name: String,
    alias: String,
    country: Option<String>,
    category: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ResolveAliasResponse {
    canonical_name: Option<String>,
    input: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/channel-aliases",
    tag = "aliases",
    params(
        ("country" = Option<String>, Query, description = "Filter by country"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
    ),
    responses(
        (status = 200, description = "Channel aliases", body = ChannelAliasPageResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_channel_aliases(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let country = params.get("country").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    match repo.list_channel_aliases(country, limit, offset).await {
        Ok((total, rows)) => Json(ChannelAliasPageResponse {
            total,
            items: rows.into_iter().map(ChannelAliasResponse::from).collect(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/channel-aliases",
    tag = "aliases",
    request_body = CreateChannelAliasRequest,
    responses(
        (status = 201, description = "Alias created", body = ChannelAliasResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn create_channel_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateChannelAliasRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let input = iptv_persistence::CreateChannelAliasInput {
        canonical_name: body.canonical_name,
        alias: body.alias,
        country: body.country,
        category: body.category,
    };
    match repo.create_channel_alias(&input).await {
        Ok(row) => (StatusCode::CREATED, Json(ChannelAliasResponse::from(row))).into_response(),
        Err(PersistenceError::Database(error))
            if error.to_string().contains("unique constraint") =>
        {
            ProblemDetails::new(
                StatusCode::CONFLICT,
                "alias-conflict",
                "Alias already exists",
                "an alias with that name is already configured",
            )
            .response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/channel-aliases/{alias_id}",
    tag = "aliases",
    params(("alias_id" = Uuid, Path, description = "Alias ID")),
    responses(
        (status = 204, description = "Alias deleted"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn delete_channel_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(alias_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.delete_channel_alias(alias_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "alias-not-found",
            "Alias not found",
            "no alias with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/channel-aliases/resolve",
    tag = "aliases",
    params(("name" = String, Query, description = "Channel name to resolve")),
    responses(
        (status = 200, description = "Resolved canonical name", body = ResolveAliasResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn resolve_channel_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let Some(name) = params.get("name") else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "missing-name",
            "Name parameter required",
            "provide a name query parameter to resolve",
        )
        .response();
    };
    match repo.resolve_channel_alias(name).await {
        Ok(canonical) => Json(ResolveAliasResponse {
            canonical_name: canonical,
            input: name.clone(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RegionSettingsDto {
    timezone: String,
    enabled_prefixes: Vec<String>,
    suggested_prefixes: Vec<String>,
    auto_detected: bool,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RegionSettingsResponse {
    settings: RegionSettingsDto,
    prefixes: Vec<RegionPrefixResponse>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RegionPrefixResponse {
    prefix: String,
    group_count: i64,
    channel_count: i64,
    suggested: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateRegionSettingsRequest {
    timezone: String,
    enabled_prefixes: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ApplyRegionFilterRequest {
    enabled_prefixes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct RegionFilterResponse {
    enabled: i64,
    disabled: i64,
}

#[utoipa::path(
    get,
    path = "/api/v1/region-settings",
    tag = "configuration",
    responses(
        (status = 200, body = RegionSettingsResponse, description = "Current region settings and available prefixes"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn get_region_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    let settings = match catalog.get_region_settings().await {
        Ok(s) => s,
        Err(error) => return persistence_error_response(error),
    };
    let prefixes = match catalog.list_region_prefixes().await {
        Ok(p) => p,
        Err(error) => return persistence_error_response(error),
    };
    let suggested = iptv_domain::suggested_prefixes_for_timezone(&settings.timezone);
    let suggested_set: std::collections::HashSet<&str> = suggested.iter().copied().collect();
    let prefix_responses = prefixes
        .iter()
        .map(|p| RegionPrefixResponse {
            prefix: p.prefix.clone(),
            group_count: p.group_count,
            channel_count: p.channel_count,
            suggested: suggested_set.contains(p.prefix.as_str()),
        })
        .collect();
    Json(RegionSettingsResponse {
        settings: RegionSettingsDto {
            timezone: settings.timezone,
            enabled_prefixes: settings.enabled_prefixes,
            suggested_prefixes: suggested.iter().map(|s| (*s).to_owned()).collect(),
            auto_detected: settings.auto_detected,
        },
        prefixes: prefix_responses,
    })
    .into_response()
}

#[utoipa::path(
    put,
    path = "/api/v1/region-settings",
    tag = "configuration",
    request_body = UpdateRegionSettingsRequest,
    responses(
        (status = 200, body = RegionSettingsDto, description = "Updated region settings"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn update_region_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<UpdateRegionSettingsRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-region-settings",
            "Invalid region settings request",
            "send a JSON request with timezone and enabledPrefixes",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog
        .update_region_settings(&request.timezone, &request.enabled_prefixes)
        .await
    {
        Ok(settings) => {
            let suggested = iptv_domain::suggested_prefixes_for_timezone(&settings.timezone);
            Json(RegionSettingsDto {
                timezone: settings.timezone,
                enabled_prefixes: settings.enabled_prefixes,
                suggested_prefixes: suggested.iter().map(|s| (*s).to_owned()).collect(),
                auto_detected: settings.auto_detected,
            })
            .into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/region-settings/apply",
    tag = "configuration",
    request_body = ApplyRegionFilterRequest,
    responses(
        (status = 200, body = RegionFilterResponse, description = "Region filter applied"),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 503, body = ProblemDetails)
    )
)]
async fn apply_region_filter(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ApplyRegionFilterRequest>, JsonRejection>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Ok(Json(request)) = request else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-region-filter",
            "Invalid region filter request",
            "send a JSON request with enabledPrefixes",
        )
        .response();
    };
    let Some(catalog) = &state.catalog_repository else {
        return persistence_unavailable();
    };
    match catalog.apply_region_filter(&request.enabled_prefixes).await {
        Ok(result) => {
            // Invalidate cached queries so the UI reflects the changes.
            Json(RegionFilterResponse {
                enabled: result.enabled,
                disabled: result.disabled,
            })
            .into_response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct RecordingRuleResponse {
    id: String,
    name: String,
    channel_id: String,
    rule_type: String,
    title_filter: Option<String>,
    category_filter: Option<String>,
    start_padding_minutes: i32,
    end_padding_minutes: i32,
    max_recordings: Option<i32>,
    keep_until: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl From<iptv_persistence::RecordingRuleRow> for RecordingRuleResponse {
    fn from(row: iptv_persistence::RecordingRuleRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name,
            channel_id: row.channel_id.to_string(),
            rule_type: row.rule_type,
            title_filter: row.title_filter,
            category_filter: row.category_filter,
            start_padding_minutes: row.start_padding_minutes,
            end_padding_minutes: row.end_padding_minutes,
            max_recordings: row.max_recordings,
            keep_until: row.keep_until,
            enabled: row.enabled,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct CreateRecordingRuleRequest {
    name: String,
    channel_id: String,
    rule_type: Option<String>,
    title_filter: Option<String>,
    category_filter: Option<String>,
    start_padding_minutes: Option<i32>,
    end_padding_minutes: Option<i32>,
    max_recordings: Option<i32>,
    keep_until: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct RecordingResponse {
    id: String,
    rule_id: Option<String>,
    channel_id: String,
    programme_id: Option<String>,
    title: String,
    description: Option<String>,
    starts_at: String,
    ends_at: String,
    status: String,
    file_path: Option<String>,
    file_size_bytes: Option<i64>,
    duration_seconds: Option<i32>,
    error_message: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<iptv_persistence::RecordingRow> for RecordingResponse {
    fn from(row: iptv_persistence::RecordingRow) -> Self {
        Self {
            id: row.id.to_string(),
            rule_id: row.rule_id.map(|id| id.to_string()),
            channel_id: row.channel_id.to_string(),
            programme_id: row.programme_id.map(|id| id.to_string()),
            title: row.title,
            description: row.description,
            starts_at: row.starts_at.to_rfc3339(),
            ends_at: row.ends_at.to_rfc3339(),
            status: row.status,
            file_path: row.file_path,
            file_size_bytes: row.file_size_bytes,
            duration_seconds: row.duration_seconds,
            error_message: row.error_message,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct CreateRecordingRequest {
    rule_id: Option<String>,
    channel_id: String,
    programme_id: Option<String>,
    title: String,
    description: Option<String>,
    starts_at: String,
    ends_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct RecordingPageResponse {
    total: i64,
    items: Vec<RecordingResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
struct RecordingStatsResponse {
    scheduled: i64,
    recording: i64,
    completed: i64,
    failed: i64,
    total_bytes: i64,
}

impl From<iptv_persistence::RecordingStats> for RecordingStatsResponse {
    fn from(stats: iptv_persistence::RecordingStats) -> Self {
        Self {
            scheduled: stats.scheduled,
            recording: stats.recording,
            completed: stats.completed,
            failed: stats.failed,
            total_bytes: stats.total_bytes,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/recordings/rules",
    tag = "recordings",
    responses(
        (status = 200, description = "Recording rules", body = Vec<RecordingRuleResponse>),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_recording_rules(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.list_recording_rules().await {
        Ok(rules) => Json(
            rules
                .into_iter()
                .map(RecordingRuleResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/recordings/rules",
    tag = "recordings",
    request_body = CreateRecordingRuleRequest,
    responses(
        (status = 201, description = "Recording rule created", body = RecordingRuleResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn create_recording_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateRecordingRuleRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let Ok(channel_id) = body.channel_id.parse::<Uuid>() else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "the channel_id must be a valid UUID",
        )
        .response();
    };
    let rule_type = body.rule_type.as_deref().unwrap_or("one-time");
    if !matches!(rule_type, "one-time" | "recurring" | "series") {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-rule-type",
            "Invalid rule type",
            "rule_type must be one-time, recurring, or series",
        )
        .response();
    }
    let keep_until = body.keep_until.as_deref().unwrap_or("space-needed");
    if !matches!(
        keep_until,
        "space-needed" | "one-day" | "one-week" | "until-watched" | "forever"
    ) {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-keep-until",
            "Invalid keep-until value",
            "keep_until must be space-needed, one-day, one-week, until-watched, or forever",
        )
        .response();
    }
    let input = iptv_persistence::CreateRecordingRuleInput {
        name: body.name,
        channel_id,
        rule_type: rule_type.to_owned(),
        title_filter: body.title_filter,
        category_filter: body.category_filter,
        start_padding_minutes: body.start_padding_minutes.unwrap_or(0),
        end_padding_minutes: body.end_padding_minutes.unwrap_or(0),
        max_recordings: body.max_recordings,
        keep_until: keep_until.to_owned(),
    };
    match repo.create_recording_rule(&input).await {
        Ok(rule) => (StatusCode::CREATED, Json(RecordingRuleResponse::from(rule))).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/recordings/rules/{rule_id}",
    tag = "recordings",
    params(("rule_id" = Uuid, Path, description = "Rule ID")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn delete_recording_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(rule_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.delete_recording_rule(rule_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "rule-not-found",
            "Rule not found",
            "no recording rule with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/recordings",
    tag = "recordings",
    params(
        ("status" = Option<String>, Query, description = "Filter by status"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
    ),
    responses(
        (status = 200, description = "Recordings", body = RecordingPageResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_recordings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let status = params.get("status").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    match repo.list_recordings(status, limit, offset).await {
        Ok((total, rows)) => Json(RecordingPageResponse {
            total,
            items: rows.into_iter().map(RecordingResponse::from).collect(),
        })
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/recordings",
    tag = "recordings",
    request_body = CreateRecordingRequest,
    responses(
        (status = 201, description = "Recording created", body = RecordingResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn create_recording(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateRecordingRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let Ok(channel_id) = body.channel_id.parse::<Uuid>() else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-channel-id",
            "Invalid channel ID",
            "the channel_id must be a valid UUID",
        )
        .response();
    };
    let rule_id = body.rule_id.and_then(|s| s.parse::<Uuid>().ok());
    let programme_id = body.programme_id.and_then(|s| s.parse::<Uuid>().ok());
    let Ok(starts_at) = body.starts_at.parse::<chrono::DateTime<chrono::Utc>>() else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-starts-at",
            "Invalid start time",
            "starts_at must be a valid ISO 8601 datetime",
        )
        .response();
    };
    let Ok(ends_at) = body.ends_at.parse::<chrono::DateTime<chrono::Utc>>() else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-ends-at",
            "Invalid end time",
            "ends_at must be a valid ISO 8601 datetime",
        )
        .response();
    };
    let input = iptv_persistence::CreateRecordingInput {
        rule_id,
        channel_id,
        programme_id,
        title: body.title,
        description: body.description,
        starts_at,
        ends_at,
    };
    match repo.create_recording(&input).await {
        Ok(recording) => (
            StatusCode::CREATED,
            Json(RecordingResponse::from(recording)),
        )
            .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/recordings/{recording_id}",
    tag = "recordings",
    params(("recording_id" = Uuid, Path, description = "Recording ID")),
    responses(
        (status = 204, description = "Recording deleted"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn delete_recording(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recording_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.delete_recording(recording_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "recording-not-found",
            "Recording not found",
            "no recording with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/recordings/stats",
    tag = "recordings",
    responses(
        (status = 200, description = "Recording statistics", body = RecordingStatsResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn recording_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.recording_stats().await {
        Ok(result) => Json(RecordingStatsResponse::from(result)).into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct StreamProfileResponse {
    id: String,
    name: String,
    profile_type: String,
    command: Option<String>,
    arguments: serde_json::Value,
    buffer_seconds: f32,
    user_agent: Option<String>,
    referer: Option<String>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl From<iptv_persistence::StreamProfileRow> for StreamProfileResponse {
    fn from(row: iptv_persistence::StreamProfileRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name,
            profile_type: row.profile_type,
            command: row.command,
            arguments: row.arguments,
            buffer_seconds: row.buffer_seconds,
            user_agent: row.user_agent,
            referer: row.referer,
            enabled: row.enabled,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct CreateStreamProfileRequest {
    name: String,
    profile_type: Option<String>,
    command: Option<String>,
    arguments: Option<serde_json::Value>,
    buffer_seconds: Option<f32>,
    user_agent: Option<String>,
    referer: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct AssignStreamProfileRequest {
    stream_profile_id: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/stream-profiles",
    tag = "stream-profiles",
    responses(
        (status = 200, description = "Stream profiles", body = Vec<StreamProfileResponse>),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn list_stream_profiles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.list_stream_profiles().await {
        Ok(profiles) => Json(
            profiles
                .into_iter()
                .map(StreamProfileResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/stream-profiles",
    tag = "stream-profiles",
    request_body = CreateStreamProfileRequest,
    responses(
        (status = 201, description = "Profile created", body = StreamProfileResponse),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn create_stream_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateStreamProfileRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let profile_type = body.profile_type.as_deref().unwrap_or("direct");
    if !matches!(
        profile_type,
        "direct" | "ffmpeg" | "vlc" | "streamlink" | "custom"
    ) {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-profile-type",
            "Invalid profile type",
            "profile_type must be direct, ffmpeg, vlc, streamlink, or custom",
        )
        .response();
    }
    let input = iptv_persistence::CreateStreamProfileInput {
        name: body.name,
        profile_type: profile_type.to_owned(),
        command: body.command,
        arguments: body.arguments.unwrap_or(serde_json::json!([])),
        buffer_seconds: body.buffer_seconds.unwrap_or(0.0),
        user_agent: body.user_agent,
        referer: body.referer,
    };
    match repo.create_stream_profile(&input).await {
        Ok(profile) => (
            StatusCode::CREATED,
            Json(StreamProfileResponse::from(profile)),
        )
            .into_response(),
        Err(PersistenceError::Database(error))
            if error.to_string().contains("unique constraint") =>
        {
            ProblemDetails::new(
                StatusCode::CONFLICT,
                "profile-conflict",
                "Profile already exists",
                "a profile with that name is already configured",
            )
            .response()
        }
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/stream-profiles/{profile_id}",
    tag = "stream-profiles",
    params(("profile_id" = Uuid, Path, description = "Profile ID")),
    responses(
        (status = 204, description = "Profile deleted"),
        (status = 401, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn delete_stream_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(profile_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    match repo.delete_stream_profile(profile_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ProblemDetails::new(
            StatusCode::NOT_FOUND,
            "profile-not-found",
            "Profile not found",
            "no stream profile with that ID exists",
        )
        .response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/channels/{channel_id}/stream-profile",
    tag = "stream-profiles",
    request_body = AssignStreamProfileRequest,
    params(("channel_id" = Uuid, Path, description = "Channel ID")),
    responses(
        (status = 204, description = "Profile assigned"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn assign_stream_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<Uuid>,
    Json(body): Json<AssignStreamProfileRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    let Ok(profile_id) = body.stream_profile_id.parse::<Uuid>() else {
        return ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid-profile-id",
            "Invalid profile ID",
            "the stream_profile_id must be a valid UUID",
        )
        .response();
    };
    match repo.assign_stream_profile(channel_id, profile_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/channels/{channel_id}/stream-profile",
    tag = "stream-profiles",
    params(("channel_id" = Uuid, Path, description = "Channel ID")),
    responses(
        (status = 204, description = "Profile removed"),
        (status = 401, body = ProblemDetails),
        (status = 503, body = ProblemDetails),
    )
)]
async fn remove_stream_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<Uuid>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let Some(repo) = state.catalog_repository.as_ref() else {
        return persistence_unavailable();
    };
    // Remove all stream profile assignments for this channel.
    match repo.remove_stream_profile(channel_id, Uuid::nil()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => persistence_error_response(error),
    }
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    require_scope(state, headers, OPERATOR_TOKEN_READ_SCOPE, false)
}

fn require_admin_mutation(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    require_scope(state, headers, OPERATOR_TOKEN_CONTROL_SCOPE, true)
}

fn require_scope(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    require_csrf: bool,
) -> Option<Response> {
    if state
        .auth
        .authorize_scope(headers, require_csrf, scope)
        .is_some()
    {
        return None;
    }
    let any_authorization = state.auth.authorize(headers, false);
    Some(match any_authorization {
        Some(crate::auth::Authorization::OperatorToken) => ProblemDetails::new(
            StatusCode::FORBIDDEN,
            "insufficient-scope",
            "Insufficient token scope",
            format!("the operator API token does not grant the {scope} scope"),
        )
        .response(),
        Some(crate::auth::Authorization::Session) if require_csrf => ProblemDetails::new(
            StatusCode::FORBIDDEN,
            "csrf-validation-failed",
            "CSRF validation failed",
            "provide the CSRF token associated with the administrator session",
        )
        .response(),
        Some(_) => ProblemDetails::new(
            StatusCode::FORBIDDEN,
            "insufficient-scope",
            "Insufficient token scope",
            format!("the authorization does not grant the {scope} scope"),
        )
        .response(),
        None => ProblemDetails::new(
            StatusCode::UNAUTHORIZED,
            "authentication-required",
            "Authentication required",
            "sign in as the local administrator or provide the bootstrap bearer token",
        )
        .response(),
    })
}

fn valid_output_token(state: &AppState, token: &str) -> bool {
    constant_time_eq(&token_hash(token), &state.output_token_hash)
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn output_token_fingerprint(token: &str) -> String {
    let hash = token_hash(token);
    hash[..8]
        .iter()
        .fold(String::with_capacity(16), |mut output, value| {
            write!(&mut output, "{value:02X}").expect("writing to a String cannot fail");
            output
        })
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn text_response(content_type: &'static str, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn no_store_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("0"));
    headers.insert(
        HeaderName::from_static("surrogate-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

fn m3u_escape(value: &str) -> String {
    value.replace(['\r', '\n'], " ").replace('"', "'")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn natural_number_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use axum::{Router, body::Body, http::Request};
    use http_body_util::BodyExt;
    use iptv_media::RingSnapshot;
    use tower::ServiceExt;
    use utoipa::PartialSchema;

    use super::*;

    fn state() -> AppState {
        state_with_database(None)
    }

    fn state_with_database(database: Option<Database>) -> AppState {
        static PASSWORD_HASH: OnceLock<String> = OnceLock::new();
        AppState::new(
            database,
            AppConfig {
                public_base_url: "http://gateway.test".into(),
                output_token: "output-secret".into(),
                admin_bootstrap_token: "admin-secret".into(),
                admin_password_hash: PASSWORD_HASH
                    .get_or_init(|| hash_admin_password("admin-password").unwrap())
                    .clone(),
                master_key: MasterKey::from_bytes([7_u8; 32]),
                tuner_count: 3,
                runtime_versions: RuntimeVersions::default(),
                oidc: None,
            },
        )
    }

    fn state_with_oidc(issuer_url: String) -> AppState {
        static PASSWORD_HASH: OnceLock<String> = OnceLock::new();
        let oidc = OidcConfig::new(
            issuer_url,
            "gateway-client",
            None,
            "http://localhost:8080/api/v1/auth/oidc/callback",
            vec!["subject-1".to_owned()],
            vec![],
            vec![],
        )
        .expect("valid OIDC test configuration");
        AppState::new(
            None,
            AppConfig {
                public_base_url: "http://gateway.test".into(),
                output_token: "output-secret".into(),
                admin_bootstrap_token: "admin-secret".into(),
                admin_password_hash: PASSWORD_HASH
                    .get_or_init(|| hash_admin_password("admin-password").unwrap())
                    .clone(),
                master_key: MasterKey::from_bytes([7_u8; 32]),
                tuner_count: 3,
                runtime_versions: RuntimeVersions::default(),
                oidc: Some(oidc),
            },
        )
    }

    #[tokio::test]
    async fn output_only_operator_token_cannot_open_admin_channel_stream() {
        let app_state = state();
        let token = "output-only-token";
        app_state.auth.add_operator_token(OperatorTokenRecord {
            id: Uuid::now_v7(),
            token_hash: token_hash(token),
            scopes: vec![OPERATOR_TOKEN_OUTPUT_SCOPE.to_owned()],
            expires_at: None,
        });
        let response = router(app_state)
            .oneshot(
                Request::get(format!("/api/v1/channels/{}/stream", Uuid::now_v7()))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn configured_oidc_routes_start_cancel_and_keep_local_logout_available() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let issuer = format!("http://127.0.0.1:{}/issuer", address.port());
        let provider = Router::new().route(
            "/issuer/.well-known/openid-configuration",
            get({
                let issuer = issuer.clone();
                move || {
                    let issuer = issuer.clone();
                    async move {
                        Json(serde_json::json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "jwks_uri": format!("{issuer}/keys"),
                            "token_endpoint_auth_methods_supported": ["none"]
                        }))
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });

        let app = router(state_with_oidc(issuer));
        let status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status_body: serde_json::Value =
            serde_json::from_str(&response_text(status).await).unwrap();
        assert_eq!(
            status_body,
            serde_json::json!({"authenticated": false, "oidcEnabled": true})
        );

        let start = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/oidc/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::TEMPORARY_REDIRECT);
        assert!(start.headers().get(header::LOCATION).is_some());
        let state_cookie = response_cookies(&start)
            .into_iter()
            .find(|cookie| cookie.starts_with("iptv_oidc_state="))
            .expect("OIDC start sets a state cookie");
        let state = cookie_value(std::slice::from_ref(&state_cookie), "iptv_oidc_state");

        let callback = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/auth/oidc/callback?error=access_denied&state={state}"
                ))
                .header(header::COOKIE, state_cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(callback.status(), StatusCode::BAD_REQUEST);
        assert_eq!(callback.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response_cookies(&callback)
                .iter()
                .any(|cookie| cookie.starts_with("iptv_oidc_state=;"))
        );
        let callback_body: serde_json::Value =
            serde_json::from_str(&response_text(callback).await).unwrap();
        assert_eq!(callback_body["code"], "oidc-invalid-callback");

        let pre_login = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let pre_login_cookies = response_cookies(&pre_login);
        let pre_login_csrf = cookie_value(&pre_login_cookies, "iptv_csrf").to_owned();
        let login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, request_cookie_header(&pre_login_cookies))
                    .header("x-csrf-token", &pre_login_csrf)
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let login_cookies = response_cookies(&login);
        let login_cookie_header = request_cookie_header(&login_cookies);
        let login_csrf = cookie_value(&login_cookies, "iptv_csrf");
        let logout = app
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, login_cookie_header)
                    .header("x-csrf-token", login_csrf)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::OK);
        server.abort();
    }

    async fn response_text(response: Response) -> String {
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    async fn output_response(app: &Router, path: &str) -> Response {
        app.clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn assert_no_store(response: &Response) {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
        assert_eq!(response.headers()[header::EXPIRES], "0");
        assert_eq!(response.headers()["surrogate-control"], "no-store");
    }

    fn sample_session(
        state: SessionState,
        last_failure: Option<SessionFailureKind>,
    ) -> HttpTsSessionSnapshot {
        HttpTsSessionSnapshot {
            key: HttpTsSessionKey::new("provider-private-id", "source-safe-id", 7),
            state,
            viewer_count: 2,
            upstream_generation: 3,
            ring_lag_events: 4,
            ring_wrap_events: 5,
            overwritten_packets: 6,
            reconnect_attempts: 7,
            failover_attempts: 8,
            failure_count: 9,
            last_failure,
            ring: RingSnapshot {
                generation: 3,
                first_sequence: 10,
                next_sequence: 30,
                retained_packets: 20,
                capacity_packets: 40,
                closed: None,
            },
        }
    }

    fn response_cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect()
    }

    fn request_cookie_header(cookies: &[String]) -> String {
        cookies
            .iter()
            .filter_map(|cookie| cookie.split(';').next())
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn cookie_value<'a>(cookies: &'a [String], name: &str) -> &'a str {
        cookies
            .iter()
            .find_map(|cookie| cookie.strip_prefix(&format!("{name}=")))
            .and_then(|cookie| cookie.split(';').next())
            .unwrap()
    }

    async fn isolated_database() -> (Database, Database, String) {
        let database_url = std::env::var("IPTV_TEST_DATABASE_URL")
            .expect("IPTV_TEST_DATABASE_URL must identify the test PostgreSQL database");
        let admin = Database::connect(&database_url, 2).await.unwrap();
        let schema = format!("iptv_api_test_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        let mut isolated_url = url::Url::parse(&database_url).unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema},public"));
        let database = Database::connect(isolated_url.as_str(), 4).await.unwrap();
        database.migrate().await.unwrap();
        (admin, database, schema)
    }

    async fn drop_isolated_database(admin: &Database, database: Database, schema: String) {
        drop(database);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .unwrap();
    }

    fn admin_request(method: &str, uri: impl AsRef<str>, body: Option<String>) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(uri.as_ref())
            .header(header::AUTHORIZATION, "Bearer admin-secret");
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body)
            }
            None => Body::empty(),
        };
        request.body(body).unwrap()
    }

    fn assert_schema<T: ToSchema + PartialSchema>() {
        assert!(!T::name().is_empty());
        let _ = <T as PartialSchema>::schema();
    }

    fn assert_serializes<T: Serialize>(value: T) {
        assert!(serde_json::to_value(value).is_ok());
    }

    fn assert_deserializes<T: serde::de::DeserializeOwned>(value: serde_json::Value) {
        assert!(serde_json::from_value::<T>(value).is_ok());
    }

    #[tokio::test]
    async fn health_is_public() {
        let response = router(state())
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn internal_provider_reservation_shares_core_capacity() {
        let app_state = state();
        let app = router(app_state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/internal/v1/provider-reservations")
            .header(header::AUTHORIZATION, "Bearer admin-secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: ProviderReservationResponse =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();

        let second = Request::builder()
            .method("POST")
            .uri("/internal/v1/provider-reservations")
            .header(header::AUTHORIZATION, "Bearer admin-secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(second).await.unwrap().status(),
            StatusCode::CONFLICT
        );

        let release = Request::builder()
            .method("DELETE")
            .uri(format!(
                "/internal/v1/provider-reservations/{}",
                body.reservation_id
            ))
            .header(header::AUTHORIZATION, "Bearer admin-secret")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(release).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let missing = app
            .oneshot(admin_request(
                "DELETE",
                "/internal/v1/provider-reservations/missing-reservation",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn internal_provider_reservation_handler_direct_path_tracks_lease_lifecycle() {
        let state = state();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer admin-secret"),
        );
        let response = reserve_provider_slot(
            State(state.clone()),
            headers.clone(),
            Json(ProviderReservationRequest {
                pool_id: "direct-handler-pool".to_owned(),
                capacity: 1,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let reservation: ProviderReservationResponse = serde_json::from_slice(&body).unwrap();
        let released =
            release_provider_slot(State(state), headers, Path(reservation.reservation_id)).await;
        assert_eq!(released.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn internal_provider_reservation_rejects_unauthorized_requests() {
        let app_state = state();
        let debug = format!("{app_state:?}");
        assert!(debug.contains("internal_reservations: \"<redacted>\""));
        assert!(debug.contains("internal_token_hash: \"<redacted>\""));
        let app = router(app_state.clone());
        for request in [
            Request::post("/internal/v1/provider-reservations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
                .unwrap(),
            Request::post("/internal/v1/provider-reservations")
                .header(header::AUTHORIZATION, "Token admin-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
                .unwrap(),
            Request::post("/internal/v1/provider-reservations")
                .header(header::AUTHORIZATION, "Bearer wrong-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
                .unwrap(),
            Request::post("/internal/v1/provider-reservations")
                .header(header::AUTHORIZATION, "Basic admin-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"pool_id":"pool-a","capacity":1}"#))
                .unwrap(),
            Request::delete("/internal/v1/provider-reservations/reservation-id")
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let direct = release_provider_slot(
            State(app_state),
            HeaderMap::new(),
            Path("reservation-id".to_owned()),
        )
        .await;
        assert_eq!(direct.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn internal_provider_reservation_rejects_malformed_and_invalid_requests() {
        let app = router(state());
        let malformed = app
            .clone()
            .oneshot(
                Request::post("/internal/v1/provider-reservations")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

        for body in [
            r#"{"pool_id":"","capacity":1}"#,
            r#"{"pool_id":"   ","capacity":1}"#,
            r#"{"pool_id":"pool-a","capacity":0}"#,
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/internal/v1/provider-reservations")
                        .header(header::AUTHORIZATION, "Bearer admin-secret")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
    }

    #[tokio::test]
    async fn internal_provider_reservation_direct_release_reports_unknown_id() {
        let response = release_provider_slot(
            State(state()),
            HeaderMap::from_iter([(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer admin-secret"),
            )]),
            Path("missing-reservation".to_owned()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn provider_reservation_error_status_maps_capacity_and_identifier_exhaustion() {
        assert_eq!(
            provider_reservation_error_status(&AcquireError::AtCapacity {
                pool_id: "pool".into(),
                capacity: 1,
                active_sessions: 1,
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            provider_reservation_error_status(&AcquireError::LeaseIdExhausted),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn control_api_requires_admin_token() {
        let response = router(state())
            .oneshot(
                Request::get("/api/v1/channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }

    #[tokio::test]
    async fn support_routes_require_admin_and_return_redacted_bundle_contract() {
        let app = router(state());
        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/support/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(admin_request("GET", "/api/v1/support/bundle", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bundle: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        assert_eq!(bundle["schemaVersion"], "1");
        assert!(bundle["logs"].is_array());
        assert!(bundle["sessions"].is_array());
        assert!(!bundle.to_string().contains("output-secret"));
        assert!(!bundle.to_string().contains("admin-secret"));

        let openapi = ApiDoc::openapi();
        assert!(openapi.paths.paths.contains_key("/api/v1/support/bundle"));
        assert!(openapi.paths.paths.contains_key("/api/v1/support/logs"));
    }

    #[tokio::test]
    async fn settings_schema_is_authenticated_and_backend_owned() {
        let app = router(state());
        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/settings/schema")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::get("/api/v1/settings/schema")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let schema: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        assert!(schema.as_array().unwrap().len() >= 8);
        assert!(schema.as_array().unwrap().iter().any(|definition| {
            definition["key"] == "media.ring.duration_seconds"
                && definition["defaultValue"] == 8
                && definition["operationalEffect"].is_string()
        }));

        let openapi = ApiDoc::openapi();
        assert!(openapi.paths.paths.contains_key("/api/v1/settings/schema"));
    }

    #[tokio::test]
    async fn operator_settings_routes_require_admin_and_register_in_openapi() {
        let app = router(state());
        let unauthorized = app
            .oneshot(
                Request::get("/api/v1/settings/effective")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let openapi = ApiDoc::openapi();
        for path in [
            "/api/v1/settings/effective",
            "/api/v1/settings/overrides",
            "/api/v1/settings/overrides/global",
            "/api/v1/settings/overrides/global/revisions",
            "/api/v1/settings/overrides/global/rollback",
            "/api/v1/settings/overrides/providers/{providerId}",
            "/api/v1/settings/overrides/groups/{groupId}",
        ] {
            assert!(
                openapi.paths.paths.contains_key(path),
                "OpenAPI must register {path}"
            );
        }
        assert_schema::<EffectiveSettingsResponse>();
        assert_schema::<OperatorOverridesResponse>();
        assert_schema::<OperatorScopeResponse>();
        assert_schema::<OperatorRevisionResponse>();
        assert_serializes(OperatorSettingScope::Global);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn operator_settings_persist_precedence_etag_revisions_and_rollback() {
        if std::env::var("IPTV_TEST_DATABASE_URL").is_err() {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        }
        let (admin, database, schema) = isolated_database().await;
        let app = router(state_with_database(Some(database.clone())));

        // Global override: ring duration seconds.
        let global_put = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/v1/settings/overrides/global",
                Some(r#"{"overrides":{"media.ring.duration_seconds":12}}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(global_put.status(), StatusCode::OK);
        assert_eq!(global_put.headers()[header::ETAG], "\"1\"");
        let global_body: serde_json::Value =
            serde_json::from_str(&response_text(global_put).await).unwrap();
        assert_eq!(global_body["scope"], "global");
        assert_eq!(global_body["revision"], 1);
        assert_eq!(global_body["overrides"]["media.ring.duration_seconds"], 12);

        // Provider override: ring duration seconds (provider wins over global).
        let provider_put = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/v1/settings/overrides/providers/provider-a",
                Some(r#"{"overrides":{"media.ring.duration_seconds":16}}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(provider_put.status(), StatusCode::OK);
        assert_eq!(provider_put.headers()[header::ETAG], "\"1\"");

        // Group override: events inferred duration (group wins over provider/global).
        let group_put = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/v1/settings/overrides/groups/sports",
                Some(r#"{"overrides":{"events.inferred_duration_seconds":9000}}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(group_put.status(), StatusCode::OK);

        // Effective settings show group > provider > global > default precedence.
        let effective = app
            .clone()
            .oneshot(admin_request(
                "GET",
                "/api/v1/settings/effective?providerId=provider-a&groupId=sports",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(effective.status(), StatusCode::OK);
        let effective_body: serde_json::Value =
            serde_json::from_str(&response_text(effective).await).unwrap();
        let settings = effective_body["settings"].as_array().unwrap();
        let ring = settings
            .iter()
            .find(|s| s["definition"]["key"] == "media.ring.duration_seconds")
            .unwrap();
        assert_eq!(ring["value"], 16);
        assert_eq!(ring["inheritedFrom"]["scope"], "provider");
        assert_eq!(ring["inheritedFrom"]["id"], "provider-a");
        let duration = settings
            .iter()
            .find(|s| s["definition"]["key"] == "events.inferred_duration_seconds")
            .unwrap();
        assert_eq!(duration["value"], 9000);
        assert_eq!(duration["inheritedFrom"]["scope"], "channel_group");
        assert_eq!(duration["inheritedFrom"]["id"], "sports");
        let apply = effective_body["applyRequirements"].as_array().unwrap();
        assert!(apply.iter().any(|value| value == "immediate"));
        assert_eq!(effective_body["etag"].as_str().unwrap().len(), 16);

        // Stale If-Match returns 412 and the current ETag.
        let mut stale_request = admin_request(
            "PUT",
            "/api/v1/settings/overrides/global",
            Some(r#"{"overrides":{"media.ring.duration_seconds":20}}"#.to_owned()),
        );
        stale_request
            .headers_mut()
            .insert(header::IF_MATCH, HeaderValue::from_static("\"99\""));
        let stale_response = app.clone().oneshot(stale_request).await.unwrap();
        assert_eq!(stale_response.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(stale_response.headers()[header::ETAG], "\"1\"");
        let stale_body: serde_json::Value =
            serde_json::from_str(&response_text(stale_response).await).unwrap();
        assert_eq!(stale_body["code"], "setting-revision-conflict");

        // Fresh If-Match succeeds and bumps the revision.
        let mut fresh_request = admin_request(
            "PUT",
            "/api/v1/settings/overrides/global",
            Some(r#"{"overrides":{"media.ring.duration_seconds":20}}"#.to_owned()),
        );
        fresh_request
            .headers_mut()
            .insert(header::IF_MATCH, HeaderValue::from_static("\"1\""));
        let fresh_response = app.clone().oneshot(fresh_request).await.unwrap();
        assert_eq!(fresh_response.status(), StatusCode::OK);
        assert_eq!(fresh_response.headers()[header::ETAG], "\"2\"");

        // Revision history records both writes, newest first.
        let revisions = app
            .clone()
            .oneshot(admin_request(
                "GET",
                "/api/v1/settings/overrides/global/revisions",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(revisions.status(), StatusCode::OK);
        let revisions_body: serde_json::Value =
            serde_json::from_str(&response_text(revisions).await).unwrap();
        let revisions_array = revisions_body.as_array().unwrap();
        assert_eq!(revisions_array[0]["revision"], 2);
        assert_eq!(revisions_array[1]["revision"], 1);
        assert_eq!(
            revisions_array[0]["afterValue"]["media.ring.duration_seconds"],
            20
        );
        assert_eq!(
            revisions_array[1]["afterValue"]["media.ring.duration_seconds"],
            12
        );

        // Rollback to revision 1 restores the earlier value and bumps to 3.
        let rollback = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/settings/overrides/global/rollback",
                Some(r#"{"revision":1}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(rollback.status(), StatusCode::OK);
        assert_eq!(rollback.headers()[header::ETAG], "\"3\"");
        let rollback_body: serde_json::Value =
            serde_json::from_str(&response_text(rollback).await).unwrap();
        assert_eq!(
            rollback_body["overrides"]["media.ring.duration_seconds"],
            12
        );

        // Rollback to a missing revision returns 404.
        let missing = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/settings/overrides/global/rollback",
                Some(r#"{"revision":99}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        // Forbidden group override returns 403 before persistence.
        let forbidden = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/v1/settings/overrides/groups/sports",
                Some(r#"{"overrides":{"media.ring.max_bytes":1048576}}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    async fn operator_settings_scope_etag_survives_empty_override_set() {
        if std::env::var("IPTV_TEST_DATABASE_URL").is_err() {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        }
        let (admin, database, schema) = isolated_database().await;
        let app = router(state_with_database(Some(database.clone())));

        // Seed one global override so the scope has a recorded revision.
        let seed = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/v1/settings/overrides/global",
                Some(r#"{"overrides":{"media.ring.duration_seconds":12}}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(seed.status(), StatusCode::OK);
        assert_eq!(seed.headers()[header::ETAG], "\"1\"");

        // Clear the scope with the seeded ETag. The revisions table keeps
        // revision 2 even though the override rows are deleted.
        let mut clear_request = admin_request(
            "PUT",
            "/api/v1/settings/overrides/global",
            Some(r#"{"overrides":{}}"#.to_owned()),
        );
        clear_request
            .headers_mut()
            .insert(header::IF_MATCH, HeaderValue::from_static("\"1\""));
        let cleared = app.clone().oneshot(clear_request).await.unwrap();
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(cleared.headers()[header::ETAG], "\"2\"");
        let cleared_body: serde_json::Value =
            serde_json::from_str(&response_text(cleared).await).unwrap();
        assert_eq!(cleared_body["revision"], 2);
        assert!(cleared_body["overrides"].as_object().unwrap().is_empty());

        // The GET scope response must report revision 2 and an ETag that
        // matches the write, not revision 0 from the empty override set.
        let loaded = app
            .clone()
            .oneshot(admin_request(
                "GET",
                "/api/v1/settings/overrides/global",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(loaded.status(), StatusCode::OK);
        assert_eq!(loaded.headers()[header::ETAG], "\"2\"");
        let loaded_body: serde_json::Value =
            serde_json::from_str(&response_text(loaded).await).unwrap();
        assert_eq!(loaded_body["revision"], 2);
        assert!(loaded_body["overrides"].as_object().unwrap().is_empty());

        // A fresh If-Match write with the loaded ETag must succeed and bump
        // the revision to 3.
        let mut fresh_request = admin_request(
            "PUT",
            "/api/v1/settings/overrides/global",
            Some(r#"{"overrides":{"media.ring.duration_seconds":30}}"#.to_owned()),
        );
        fresh_request
            .headers_mut()
            .insert(header::IF_MATCH, HeaderValue::from_static("\"2\""));
        let fresh = app.clone().oneshot(fresh_request).await.unwrap();
        assert_eq!(fresh.status(), StatusCode::OK);
        assert_eq!(fresh.headers()[header::ETAG], "\"3\"");

        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn reconciliation_records_revisions_and_rollback_restores_state() {
        if std::env::var("IPTV_TEST_DATABASE_URL").is_err() {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        }
        let (admin, database, schema) = isolated_database().await;
        let app = router(state_with_database(Some(database.clone())));
        let pool = database.pool().clone();

        // Seed one provider account with one active M3U snapshot and one
        // supported stream so reconciliation produces one canonical channel.
        let account_id = uuid::Uuid::now_v7();
        let snapshot_id = uuid::Uuid::now_v7();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
        )
        .bind(account_id)
        .bind(format!("Rollback account {}", uuid::Uuid::now_v7()))
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, 'news', 'News HD', 'news.tvg', '7.1', 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(snapshot_id)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        // First reconciliation records revision 1.
        let first = app
            .clone()
            .oneshot(admin_request("POST", "/api/v1/epg/reconcile", None))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        // The per-account reconcile is invoked through the persistence layer
        // directly because the public API exposes only the global EPG
        // reconcile. The rollback endpoint targets the account revisions.
        let catalog = iptv_persistence::CatalogRepository::new(pool.clone());
        catalog
            .reconcile_provider_account(account_id)
            .await
            .unwrap();

        // Revision history lists the recorded reconciliation revision.
        let revisions = app
            .clone()
            .oneshot(admin_request(
                "GET",
                format!("/api/v1/sources/{account_id}/reconcile/revisions"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(revisions.status(), StatusCode::OK);
        let revisions_body: serde_json::Value =
            serde_json::from_str(&response_text(revisions).await).unwrap();
        let revisions_array = revisions_body.as_array().unwrap();
        assert_eq!(revisions_array[0]["revision"], 1);
        assert!(revisions_array[0]["afterValue"]["channels"].is_array());

        // Mutate the catalog so a second reconciliation removes the channel.
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("UPDATE provider_streams SET supported = false WHERE snapshot_id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        catalog
            .reconcile_provider_account(account_id)
            .await
            .unwrap();

        // The channel is gone after the second reconciliation.
        let channel_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic'",
        )
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(channel_count, 0);

        // Rollback to revision 1 restores the channel, stream link, and EPG
        // mapping snapshot captured before the second reconciliation.
        let rollback = app
            .clone()
            .oneshot(admin_request(
                "POST",
                format!("/api/v1/sources/{account_id}/reconcile/rollback"),
                Some(r#"{"revision":1}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(rollback.status(), StatusCode::OK);
        let rollback_body: serde_json::Value =
            serde_json::from_str(&response_text(rollback).await).unwrap();
        assert_eq!(rollback_body["targetRevision"], 1);
        assert_eq!(rollback_body["channelsRestored"], 1);

        let restored_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic'",
        )
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored_count, 1);

        // Rollback to a missing revision returns 404.
        let missing = app
            .clone()
            .oneshot(admin_request(
                "POST",
                format!("/api/v1/sources/{account_id}/reconcile/rollback"),
                Some(r#"{"revision":99}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        // An invalid source id returns 400.
        let bad = app
            .clone()
            .oneshot(admin_request(
                "GET",
                "/api/v1/sources/not-a-uuid/reconcile/revisions",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        drop_isolated_database(&admin, database, schema).await;
    }

    #[test]
    fn aggregate_session_contract_maps_every_typed_state_and_failure() {
        let pool = PoolSnapshot {
            pool_id: "provider-private-id".into(),
            capacity: 3,
            active_sessions: 1,
            high_watermark: 2,
            available_slots: 2,
        };
        for (state, expected) in [
            (SessionState::Idle, "idle"),
            (SessionState::Reserving, "reserving"),
            (SessionState::Starting, "starting"),
            (SessionState::Priming, "priming"),
            (SessionState::Streaming, "streaming"),
            (SessionState::Recovering, "recovering"),
            (SessionState::FailingOver, "failing-over"),
            (SessionState::Stopping, "stopping"),
            (SessionState::Failed, "failed"),
        ] {
            assert_eq!(session_state_name(state), expected);
        }
        for (failure, expected) in [
            (SessionFailureKind::UpstreamEnded, "upstream-ended"),
            (SessionFailureKind::Http, "http"),
            (SessionFailureKind::Packetization, "packetization"),
            (SessionFailureKind::Priming, "priming"),
            (SessionFailureKind::RecoveryExpired, "recovery-expired"),
        ] {
            assert_eq!(session_failure_name(failure), expected);
        }

        let response = session_response(
            &sample_session(SessionState::Recovering, Some(SessionFailureKind::Http)),
            Some(&pool),
        );
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["providerPoolId"], "provider-private-id");
        assert_eq!(json["sourceId"], "source-safe-id");
        assert_eq!(json["configuredGeneration"], 7);
        assert_eq!(json["upstreamGeneration"], 3);
        assert_eq!(json["viewerCount"], 2);
        assert_eq!(json["retainedPackets"], 20);
        assert_eq!(json["providerCapacity"], 3);
        assert_eq!(json["lastFailure"], "http");
        assert!(json.get("endpoint").is_none());

        let without_pool = session_response(&sample_session(SessionState::Idle, None), None);
        assert_eq!(without_pool.provider_capacity, 0);
        assert!(without_pool.last_failure.is_none());
    }

    #[test]
    fn prometheus_metrics_are_aggregate_and_identifier_free() {
        let sessions = [
            sample_session(SessionState::Streaming, None),
            sample_session(SessionState::Recovering, Some(SessionFailureKind::Http)),
        ];
        let pools = [PoolSnapshot {
            pool_id: "provider-private-id".into(),
            capacity: 3,
            active_sessions: 2,
            high_watermark: 3,
            available_slots: 1,
        }];
        let values = media_metric_values(&sessions, &pools);
        assert_eq!(values.provider_pools, 1);
        assert_eq!(values.provider_capacity, 3);
        assert_eq!(values.shared_sessions, 2);
        assert_eq!(values.viewers, 4);
        assert_eq!(values.retained_packets, 40);
        assert_eq!(values.overwritten_packets, 12);
        assert_eq!(values.failures, 18);

        let document = prometheus_media_metrics(&sessions, &pools);
        assert!(document.contains("# TYPE iptv_media_viewers gauge"));
        assert!(document.contains("iptv_media_viewers 4"));
        assert!(document.contains("iptv_media_reconnect_attempts_total 14"));
        assert!(!document.contains("provider-private-id"));
        assert!(!document.contains("source-safe-id"));
        assert!(!document.contains('{'));
    }

    #[tokio::test]
    async fn metrics_and_sse_expose_safe_live_observability() {
        let app_state = state();
        app_state
            .set_stream_source(ChannelStreamSource {
                channel_id: Uuid::now_v7(),
                provider_pool_id: "do-not-export-provider-id".into(),
                source_id: "do-not-export-source-id".into(),
                generation: 1,
                upstream_url: "https://user:password@provider.invalid/live?token=secret".into(),
                max_connections: 3,
                estimated_bitrate_bits_per_second: 8_000_000,
            })
            .await;
        let app = router(app_state);

        let metrics = app
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(metrics.status(), StatusCode::OK);
        assert_eq!(
            metrics.headers()[header::CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let metrics = response_text(metrics).await;
        assert!(metrics.contains("iptv_media_provider_capacity 3"));
        for sensitive in ["do-not-export", "password", "token", "secret"] {
            assert!(!metrics.contains(sensitive));
        }

        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/session-events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let events = app
            .oneshot(
                Request::get("/api/v1/session-events")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events.status(), StatusCode::OK);
        assert_eq!(events.headers()[header::CONTENT_TYPE], "text/event-stream");
        assert_eq!(
            events.headers()[header::CACHE_CONTROL],
            "no-cache, no-transform"
        );
        assert_eq!(events.headers()[header::CONTENT_ENCODING], "identity");
        assert_eq!(events.headers()["x-accel-buffering"], "no");
        let frame = tokio::time::timeout(Duration::from_secs(1), events.into_body().frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let data = frame.into_data().unwrap();
        let event = String::from_utf8(data.to_vec()).unwrap();
        assert!(event.starts_with("event: sessions\n"));
        assert!(event.contains("data: []\n"));

        let openapi = ApiDoc::openapi();
        assert!(openapi.paths.paths.contains_key("/api/v1/sessions"));
        assert!(openapi.paths.paths.contains_key("/api/v1/session-events"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn durable_source_and_job_routes_use_postgres_when_available() {
        let Ok(database_url) = std::env::var("IPTV_TEST_DATABASE_URL") else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        };
        let database = Database::connect(&database_url, 4).await.unwrap();
        database.migrate().await.unwrap();
        let pool = database.pool().clone();
        let app = router(state_with_database(Some(database)));
        let suffix = Uuid::now_v7();
        let endpoint =
            "https://alice:password@provider.test/live/alice/secret/list.m3u?token=value";
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"name":"API encrypted {suffix}","kind":"M3U","endpoint":"{endpoint}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: serde_json::Value =
            serde_json::from_str(&response_text(created).await).unwrap();
        let source_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
        assert_eq!(created["state"], "syncing");
        for secret in ["alice", "password", "secret", "token", "value"] {
            assert!(!created["endpoint"].as_str().unwrap().contains(secret));
        }

        let sources = app
            .clone()
            .oneshot(
                Request::get("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(sources.status(), StatusCode::OK);
        let sources: serde_json::Value =
            serde_json::from_str(&response_text(sources).await).unwrap();
        assert!(sources.as_array().unwrap().iter().any(|source| {
            source["id"] == created["id"] && source["endpoint"] == created["endpoint"]
        }));

        let jobs = app
            .clone()
            .oneshot(
                Request::get("/api/v1/jobs")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(jobs.status(), StatusCode::OK);
        let jobs: serde_json::Value = serde_json::from_str(&response_text(jobs).await).unwrap();
        assert!(
            jobs.as_array()
                .unwrap()
                .iter()
                .any(|job| { job["kind"] == "refresh-source" && job["status"] == "queued" })
        );
        let job_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE kind = 'refresh-source' AND payload->>'sourceId' = $1",
        )
        .bind(source_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        let cancelled = app
            .oneshot(
                Request::post(format!("/api/v1/jobs/{job_id}/cancel"))
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response_text(cancelled).await).unwrap(),
            serde_json::json!({"ok": true, "message": "Cancellation requested."})
        );

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("DELETE FROM jobs WHERE payload->>'sourceId' = $1")
            .bind(source_id.to_string())
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM audit_events WHERE resource_id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn output_routes_enforce_database_profile_tokens_and_channel_selection() {
        let Ok(database_url) = std::env::var("IPTV_TEST_DATABASE_URL") else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        };
        let admin = Database::connect(&database_url, 2).await.unwrap();
        let schema = format!("iptv_api_test_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        let mut isolated_url = url::Url::parse(&database_url).unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema},public"));
        let database = Database::connect(isolated_url.as_str(), 4).await.unwrap();
        database.migrate().await.unwrap();
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());
        let state = state_with_database(Some(database.clone()));
        state.initialize_output_profile().await.unwrap();

        let included_channel_id = Uuid::now_v7();
        let excluded_channel_id = Uuid::now_v7();
        for (id, number, name) in [
            (included_channel_id, "10", "Included channel"),
            (excluded_channel_id, "20", "Excluded channel"),
        ] {
            sqlx::query("INSERT INTO channels (id, channel_number, name) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(number)
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE output_profiles SET include_all_channels = false WHERE id = $1")
            .bind(iptv_persistence::ENVIRONMENT_OUTPUT_PROFILE_ID)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO output_profile_channels (output_profile_id, channel_id, position) VALUES ($1, $2, 0)",
        )
        .bind(iptv_persistence::ENVIRONMENT_OUTPUT_PROFILE_ID)
        .bind(included_channel_id)
        .execute(&pool)
        .await
        .unwrap();

        let app = router(state);
        let playlist_response = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/playlist.m3u")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(playlist_response.status(), StatusCode::OK);
        assert_no_store(&playlist_response);
        let playlist_body = response_text(playlist_response).await;
        assert!(playlist_body.contains("Included channel"));
        assert!(!playlist_body.contains("Excluded channel"));
        assert!(playlist_body.contains(&included_channel_id.to_string()));
        assert!(!playlist_body.contains(&excluded_channel_id.to_string()));

        let excluded_stream = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/out/output-secret/stream/{excluded_channel_id}.ts"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(excluded_stream.status(), StatusCode::NOT_FOUND);

        let discover = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(discover.status(), StatusCode::OK);
        assert_no_store(&discover);
        let discover: serde_json::Value =
            serde_json::from_str(&response_text(discover).await).unwrap();
        assert_eq!(discover["TunerCount"], 3);

        let xmltv = output_response(&app, "/out/output-secret/xmltv.xml").await;
        assert_eq!(xmltv.status(), StatusCode::OK);
        assert_no_store(&xmltv);
        let lineup = output_response(&app, "/out/output-secret/hdhr/lineup.json").await;
        assert_eq!(lineup.status(), StatusCode::OK);
        assert_no_store(&lineup);
        let lineup_status =
            output_response(&app, "/out/output-secret/hdhr/lineup_status.json").await;
        assert_eq!(lineup_status.status(), StatusCode::OK);
        assert_no_store(&lineup_status);
        let device = output_response(&app, "/out/output-secret/hdhr/device.xml").await;
        assert_eq!(device.status(), StatusCode::OK);
        assert_no_store(&device);

        // Update only the tuner count through the startup path. The persisted
        // current token hash stays as the environment hash so `output-secret`
        // keeps resolving. `ensure_environment_output_profile` must not rotate.
        catalog
            .ensure_environment_output_profile(
                OutputProfileTokenHash::from_sha256(token_hash("output-secret")),
                5,
            )
            .await
            .unwrap();
        let tuned = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tuned.status(), StatusCode::OK);
        let tuned_body: serde_json::Value =
            serde_json::from_str(&response_text(tuned).await).unwrap();
        assert_eq!(tuned_body["TunerCount"], 5);

        // Rotate through the dedicated rotation path. The prior hash stays
        // valid through the overlap window so both tokens resolve. The tuner
        // count is unchanged by rotation.
        let rotated_token = "rotated-output-secret";
        catalog
            .rotate_environment_output_profile_token(
                OutputProfileTokenHash::from_sha256(token_hash(rotated_token)),
                300,
            )
            .await
            .unwrap();
        for token in ["output-secret", rotated_token] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/out/{token}/hdhr/discover.json"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_no_store(&response);
            let body: serde_json::Value =
                serde_json::from_str(&response_text(response).await).unwrap();
            assert_eq!(body["TunerCount"], 5);
        }
        let invalid_token = app
            .clone()
            .oneshot(
                Request::get("/out/invalid/playlist.m3u")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_token.status(), StatusCode::NOT_FOUND);

        drop(app);
        drop(catalog);
        drop(pool);
        drop(database);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn jellyfin_setup_becomes_regeneration_required_after_rotation_restart() {
        let Ok(database_url) = std::env::var("IPTV_TEST_DATABASE_URL") else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        };
        let admin = Database::connect(&database_url, 2).await.unwrap();
        let schema = format!("iptv_api_test_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        let mut isolated_url = url::Url::parse(&database_url).unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema},public"));
        let database = Database::connect(isolated_url.as_str(), 4).await.unwrap();
        database.migrate().await.unwrap();
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());

        // Initial startup seeds the environment hash and exposes the
        // environment plaintext token in the published URLs.
        let state = state_with_database(Some(database.clone()));
        state.initialize_output_profile().await.unwrap();
        let app = router(state.clone());
        let setup = app
            .clone()
            .oneshot(
                Request::get("/api/v1/jellyfin/setup")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(setup.status(), StatusCode::OK);
        assert_no_store(&setup);
        let setup_body: serde_json::Value =
            serde_json::from_str(&response_text(setup).await).unwrap();
        assert_eq!(setup_body["status"], "available");
        assert_eq!(
            setup_body["playlistUrl"],
            "http://gateway.test/out/output-secret/playlist.m3u"
        );

        // Rotate the token through the dedicated rotation path. The plaintext
        // is returned once and is never retained by the server.
        let rotated_token = "rotated-output-secret";
        let rotated_hash = OutputProfileTokenHash::from_sha256(token_hash(rotated_token));
        catalog
            .rotate_environment_output_profile_token(rotated_hash.clone(), 300)
            .await
            .unwrap();
        // The persisted current hash is now the rotated hash, not the
        // environment hash.
        assert_eq!(
            catalog
                .environment_output_profile_token_hash()
                .await
                .unwrap()
                .as_ref()
                .map(OutputProfileTokenHash::as_bytes),
            Some(rotated_hash.as_bytes())
        );

        // Simulate a restart. The environment hash re-seeds through the
        // startup path but must not replace the rotated current hash. The
        // setup status becomes regeneration-required because the environment
        // plaintext no longer matches the active token.
        let restarted_state = state_with_database(Some(database.clone()));
        restarted_state.initialize_output_profile().await.unwrap();
        assert_eq!(
            catalog
                .environment_output_profile_token_hash()
                .await
                .unwrap()
                .as_ref()
                .map(OutputProfileTokenHash::as_bytes),
            Some(rotated_hash.as_bytes()),
            "environment hash must not become current after restart"
        );
        let restarted_app = router(restarted_state.clone());
        let restarted_setup = restarted_app
            .clone()
            .oneshot(
                Request::get("/api/v1/jellyfin/setup")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(restarted_setup.status(), StatusCode::OK);
        assert_no_store(&restarted_setup);
        let restarted_body: serde_json::Value =
            serde_json::from_str(&response_text(restarted_setup).await).unwrap();
        assert_eq!(restarted_body["status"], "regeneration-required");
        assert!(restarted_body.get("playlistUrl").is_none());
        assert!(restarted_body.get("xmltvUrl").is_none());
        assert!(restarted_body.get("hdhrDeviceUrl").is_none());
        assert_eq!(restarted_body["guideDaysMax"], 30);

        // The rotated token still serves output routes. The environment token
        // resolves only as the prior hash within the overlap window.
        let rotated_response = restarted_app
            .clone()
            .oneshot(
                Request::get("/out/rotated-output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rotated_response.status(), StatusCode::OK);
        let environment_response = restarted_app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(environment_response.status(), StatusCode::OK);

        // After the overlap window expires, the environment token stops
        // resolving while the rotated token stays active.
        sqlx::query(
            "UPDATE output_profiles
             SET previous_token_expires_at = now() - interval '1 second'
             WHERE id = $1",
        )
        .bind(iptv_persistence::ENVIRONMENT_OUTPUT_PROFILE_ID)
        .execute(&pool)
        .await
        .unwrap();
        let expired_environment = restarted_app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(expired_environment.status(), StatusCode::NOT_FOUND);
        let still_active_rotated = restarted_app
            .clone()
            .oneshot(
                Request::get("/out/rotated-output-secret/hdhr/discover.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(still_active_rotated.status(), StatusCode::OK);

        let api_rotated_response = restarted_app
            .clone()
            .oneshot(
                Request::post("/api/v1/jellyfin/setup/rotate")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(api_rotated_response.status(), StatusCode::OK);
        assert_no_store(&api_rotated_response);

        drop(restarted_app);
        drop(restarted_state);
        drop(app);
        drop(state);
        drop(catalog);
        drop(pool);
        drop(database);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn password_session_enforces_csrf_and_can_logout() {
        let app = router(state());

        let signed_out_status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(signed_out_status.status(), StatusCode::OK);
        assert_eq!(
            signed_out_status.headers()[header::CACHE_CONTROL],
            "no-store"
        );
        let pre_login_cookies = response_cookies(&signed_out_status);
        assert_eq!(pre_login_cookies.len(), 1);
        assert!(pre_login_cookies[0].starts_with("iptv_csrf="));
        assert!(!pre_login_cookies[0].contains("HttpOnly"));
        let pre_login_csrf = cookie_value(&pre_login_cookies, "iptv_csrf").to_owned();
        let pre_login_cookie_header = request_cookie_header(&pre_login_cookies);
        let body: serde_json::Value =
            serde_json::from_str(&response_text(signed_out_status).await).unwrap();
        assert_eq!(body, serde_json::json!({"authenticated": false}));

        let login_response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &pre_login_cookie_header)
                    .header("x-csrf-token", &pre_login_csrf)
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);
        let cookies = response_cookies(&login_response);
        assert_eq!(cookies.len(), 2);
        let session_cookie = cookies
            .iter()
            .find(|cookie| cookie.starts_with("iptv_session="))
            .unwrap();
        let csrf_cookie = cookies
            .iter()
            .find(|cookie| cookie.starts_with("iptv_csrf="))
            .unwrap();
        assert!(session_cookie.contains("HttpOnly"));
        assert!(!csrf_cookie.contains("HttpOnly"));
        let cookie_header = request_cookie_header(&cookies);
        let csrf = cookie_value(&cookies, "iptv_csrf");
        assert_ne!(csrf, pre_login_csrf);
        let body: serde_json::Value =
            serde_json::from_str(&response_text(login_response).await).unwrap();
        assert_eq!(body["authenticated"], true);
        assert_eq!(body["user"]["id"], "operator-1");
        assert_eq!(body["user"]["username"], "operator");
        assert_eq!(body["user"]["displayName"], "Relay operator");

        let replayed_pre_login_token = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, pre_login_cookie_header)
                    .header("x-csrf-token", pre_login_csrf)
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replayed_pre_login_token.status(), StatusCode::FORBIDDEN);

        let status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .header(header::COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        assert!(status.headers().get(header::SET_COOKIE).is_none());
        let status: serde_json::Value = serde_json::from_str(&response_text(status).await).unwrap();
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["user"]["displayName"], "Relay operator");

        let channel_body =
            r#"{"number":"99","name":"Session-created channel","group":null,"logo_url":null}"#;
        let missing_csrf = app
            .clone()
            .oneshot(
                Request::post("/api/v1/channels")
                    .header(header::COOKIE, &cookie_header)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(channel_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);

        let logout_missing_csrf = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header(header::COOKIE, &cookie_header)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout_missing_csrf.status(), StatusCode::FORBIDDEN);

        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/channels")
                    .header(header::COOKIE, &cookie_header)
                    .header("x-csrf-token", csrf)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(channel_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let logout = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header(header::COOKIE, &cookie_header)
                    .header("x-csrf-token", csrf)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::OK);
        assert_eq!(
            logout.headers().get_all(header::SET_COOKIE).iter().count(),
            2
        );
        let logout_body: serde_json::Value =
            serde_json::from_str(&response_text(logout).await).unwrap();
        assert_eq!(
            logout_body,
            serde_json::json!({"ok": true, "message": "Signed out."})
        );

        let status = app
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .header(header::COOKIE, cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(response_cookies(&status).len(), 1);
        let body: serde_json::Value = serde_json::from_str(&response_text(status).await).unwrap();
        assert_eq!(body, serde_json::json!({"authenticated": false}));
    }

    #[tokio::test]
    async fn database_login_disables_bootstrap_bearer_after_restart() {
        let Ok(database_url) = std::env::var("IPTV_TEST_DATABASE_URL") else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL API integration test");
            return;
        };
        let admin = Database::connect(&database_url, 2).await.unwrap();
        let schema = format!("iptv_api_test_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        let mut isolated_url = url::Url::parse(&database_url).unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema},public"));
        let database = Database::connect(isolated_url.as_str(), 4).await.unwrap();
        database.migrate().await.unwrap();

        let state = state_with_database(Some(database.clone()));
        state.initialize_auth().await.unwrap();
        let app = router(state);
        let bootstrap = app
            .clone()
            .oneshot(
                Request::get("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::OK);

        let status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookies = response_cookies(&status);
        let login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, request_cookie_header(&cookies))
                    .header("x-csrf-token", cookie_value(&cookies, "iptv_csrf"))
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        assert!(!database.bootstrap_bearer_enabled().await.unwrap());

        let disabled = app
            .clone()
            .oneshot(
                Request::get("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(disabled.status(), StatusCode::UNAUTHORIZED);

        let restarted_state = state_with_database(Some(database.clone()));
        restarted_state.initialize_auth().await.unwrap();
        let restarted_app = router(restarted_state);
        let after_restart = restarted_app
            .oneshot(
                Request::get("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after_restart.status(), StatusCode::UNAUTHORIZED);

        drop(app);
        drop(database);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn invalid_credentials_are_generic_and_do_not_consume_csrf() {
        let app = router(state());
        let status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookies = response_cookies(&status);
        let cookie_header = request_cookie_header(&cookies);
        let csrf = cookie_value(&cookies, "iptv_csrf");
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie_header)
                    .header("x-csrf-token", csrf)
                    .body(Body::from(r#"{"username":"unknown","password":"wrong"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        let problem: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        assert_eq!(problem["title"], "Invalid credentials");
        assert_eq!(problem["detail"], "the username or password is incorrect");

        let retry = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie_header)
                    .header("x-csrf-token", csrf)
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_failures_use_problem_details() {
        let app = router(state());
        let missing_csrf = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            missing_csrf.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let malformed = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            malformed.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let anonymous_status = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookies = response_cookies(&anonymous_status);
        let mismatched_login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, request_cookie_header(&cookies))
                    .header("x-csrf-token", "different-token")
                    .body(Body::from(
                        r#"{"username":"operator","password":"admin-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatched_login.status(), StatusCode::FORBIDDEN);

        let unauthorized_logout = app
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, request_cookie_header(&cookies))
                    .header("x-csrf-token", cookie_value(&cookies, "iptv_csrf"))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized_logout.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized_logout.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }

    #[tokio::test]
    async fn creates_channel_and_publishes_matching_m3u_xmltv_ids() {
        let app_state = state();
        let app = router(app_state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/channels")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"number":"10","name":"News & Weather","group":"News","logo_url":null}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let id = app_state.catalog.read().await.channels[0].id;
        let m3u = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/playlist.m3u")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let m3u = response_text(m3u).await;
        assert!(m3u.contains(&format!("tvg-id=\"{id}\"")));

        let xml = app
            .oneshot(
                Request::get("/out/output-secret/xmltv.xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let xml = response_text(xml).await;
        assert!(xml.contains(&format!("channel id=\"{id}\"")));
        assert!(xml.contains("News &amp; Weather"));
    }

    #[tokio::test]
    async fn invalid_output_token_looks_missing() {
        let response = router(state())
            .oneshot(
                Request::get("/out/wrong/playlist.m3u")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn preview_urls_never_contain_admin_credentials() {
        let app_state = state();
        let channel_id = Uuid::now_v7();
        app_state
            .catalog
            .write()
            .await
            .channels
            .push(ChannelRecord {
                id: channel_id,
                number: "1".into(),
                name: "Preview source".into(),
                group: None,
                logo_url: None,
                enabled: true,
            });
        app_state
            .set_stream_source(ChannelStreamSource {
                channel_id,
                provider_pool_id: "provider".into(),
                source_id: "preview".into(),
                generation: 1,
                upstream_url: "not a url".into(),
                max_connections: 1,
                estimated_bitrate_bits_per_second: 8_000_000,
            })
            .await;
        let app = router(app_state);

        let preview = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/channels/{channel_id}/preview"))
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        assert_eq!(preview.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(preview.headers()[header::REFERRER_POLICY], "no-referrer");
        let preview: serde_json::Value =
            serde_json::from_str(&response_text(preview).await).unwrap();
        assert_eq!(
            preview["streamUrl"],
            format!("/api/v1/channels/{channel_id}/stream")
        );
        assert!(!preview.to_string().contains("admin-secret"));

        let query_credential = app
            .oneshot(
                Request::get(format!(
                    "/api/v1/channels/{channel_id}/stream?token=admin-secret"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(query_credential.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn configured_stream_errors_are_retryable_and_redacted() {
        let app_state = state();
        let channel_id = Uuid::now_v7();
        app_state
            .catalog
            .write()
            .await
            .channels
            .push(ChannelRecord {
                id: channel_id,
                number: "1".into(),
                name: "Broken source".into(),
                group: None,
                logo_url: None,
                enabled: true,
            });
        app_state
            .set_stream_source(ChannelStreamSource {
                channel_id,
                provider_pool_id: "provider".into(),
                source_id: "broken".into(),
                generation: 1,
                upstream_url: "not a url?password=super-secret".into(),
                max_connections: 1,
                estimated_bitrate_bits_per_second: 8_000_000,
            })
            .await;

        let response = router(app_state)
            .oneshot(
                Request::get(format!("/out/output-secret/stream/{channel_id}.ts"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()[header::RETRY_AFTER], "5");
        let body = response_text(response).await;
        assert!(!body.contains("super-secret"));
    }

    #[tokio::test]
    async fn enabled_channel_without_source_is_retryable() {
        let app_state = state();
        let channel_id = Uuid::now_v7();
        app_state
            .catalog
            .write()
            .await
            .channels
            .push(ChannelRecord {
                id: channel_id,
                number: "2".into(),
                name: "Missing source".into(),
                group: None,
                logo_url: None,
                enabled: true,
            });
        let response = router(app_state)
            .oneshot(
                Request::get(format!("/out/output-secret/stream/{channel_id}.ts"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "5");
    }

    #[tokio::test]
    async fn duplicate_channel_number_is_conflict() {
        let app = router(state());
        let body = r#"{"number":"7","name":"First","group":null,"logo_url":null}"#;
        for expected in [StatusCode::CREATED, StatusCode::CONFLICT] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/v1/channels")
                        .header(header::AUTHORIZATION, "Bearer admin-secret")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn management_contract_returns_ui_shapes_and_validates_jellyfin() {
        let app_state = state();
        let channel_id = Uuid::now_v7();
        let starts_at = Utc::now();
        {
            let mut catalog = app_state.catalog.write().await;
            catalog.channels.push(ChannelRecord {
                id: channel_id,
                number: "7.1".into(),
                name: "Test Seven".into(),
                group: Some("Test".into()),
                logo_url: None,
                enabled: true,
            });
            catalog.programmes.push(ProgrammeRecord {
                channel_id,
                starts_at,
                stops_at: starts_at + chrono::Duration::hours(1),
                title: "Test programme".into(),
                description: None,
                categories: vec!["Testing".into()],
            });
        }
        app_state
            .set_stream_source(ChannelStreamSource {
                channel_id,
                provider_pool_id: "provider".into(),
                source_id: "source".into(),
                generation: 1,
                upstream_url: "not a URL".into(),
                max_connections: 3,
                estimated_bitrate_bits_per_second: 7_800_000,
            })
            .await;
        let app = router(app_state.clone());

        for path in [
            "/api/v1/system",
            "/api/v1/channels",
            "/api/v1/programmes",
            "/api/v1/events",
            "/api/v1/sessions",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header(header::AUTHORIZATION, "Bearer admin-secret")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(
                response.headers()[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("application/json")
            );
            let body: serde_json::Value =
                serde_json::from_str(&response_text(response).await).unwrap();
            if path == "/api/v1/system" {
                assert_eq!(body["channels"], 1);
                assert_eq!(body["providerLimit"], 3);
                assert_eq!(body["guideCoverage"], 100.0);
            } else if path == "/api/v1/channels" {
                assert_eq!(body["total"], 1);
                assert_eq!(body["items"][0]["tvgId"], channel_id.to_string());
                assert_eq!(body["items"][0]["bitrateKbps"], 7_800);
            } else if path == "/api/v1/programmes" {
                assert_eq!(body["total"], 1);
                assert_eq!(body["items"][0]["channel"], "Test Seven");
                assert_eq!(body["items"][0]["confidence"], 100);
            } else {
                assert!(body.is_array());
            }
        }

        let unavailable_sources = app
            .clone()
            .oneshot(
                Request::get("/api/v1/sources")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            unavailable_sources.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            unavailable_sources.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let unauthorized_setup = app
            .clone()
            .oneshot(
                Request::get("/api/v1/jellyfin/setup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized_setup.status(), StatusCode::UNAUTHORIZED);

        let setup = app
            .clone()
            .oneshot(
                Request::get("/api/v1/jellyfin/setup")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(setup.status(), StatusCode::OK);
        assert_no_store(&setup);
        let setup_body: serde_json::Value =
            serde_json::from_str(&response_text(setup).await).unwrap();
        assert_eq!(setup_body["status"], "available");
        assert_eq!(
            setup_body["playlistUrl"],
            "http://gateway.test/out/output-secret/playlist.m3u"
        );
        assert_eq!(
            setup_body["xmltvUrl"],
            "http://gateway.test/out/output-secret/xmltv.xml"
        );
        assert_eq!(
            setup_body["hdhrDeviceUrl"],
            "http://gateway.test/out/output-secret/hdhr/device.xml"
        );
        assert_eq!(setup_body["guideDaysMax"], 30);
        // The raw token is never exposed as a separate field.
        assert!(setup_body.get("token").is_none());
        assert!(setup_body.get("outputToken").is_none());

        // Rotation requires admin mutation authorization.
        let unauthenticated_rotate = app
            .clone()
            .oneshot(
                Request::post("/api/v1/jellyfin/setup/rotate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated_rotate.status(), StatusCode::UNAUTHORIZED);

        // Without a database the rotation endpoint reports persistence
        // unavailable instead of generating a token.
        let rotate_unavailable = app
            .oneshot(
                Request::post("/api/v1/jellyfin/setup/rotate")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rotate_unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn jellyfin_setup_regeneration_required_omits_urls() {
        let setup = JellyfinSetup::regeneration_required();
        assert_eq!(setup.status, JELLYFIN_SETUP_REGENERATION_REQUIRED);
        assert!(setup.playlist_url.is_none());
        assert!(setup.xmltv_url.is_none());
        assert!(setup.hdhr_device_url.is_none());
        assert_eq!(setup.guide_days_max, GUIDE_DAYS_MAX);
    }

    #[test]
    fn generate_output_token_produces_url_safe_base64() {
        let token = generate_output_token().expect("random source is available");
        assert!(token.len() >= 43);
        // URL-safe base64 without padding contains only these characters.
        for byte in token.bytes() {
            assert!(
                byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_',
                "unexpected byte {byte} in token"
            );
        }
    }

    #[test]
    fn escaping_prevents_output_injection() {
        assert_eq!(m3u_escape("bad\"\nname"), "bad' name");
        assert_eq!(xml_escape("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
    }

    #[test]
    fn token_comparison_is_exact() {
        let hash = token_hash("secret");
        assert!(constant_time_eq(&hash, &token_hash("secret")));
        assert!(!constant_time_eq(&hash, &token_hash("Secret")));
    }

    #[test]
    fn stream_source_debug_redacts_credentials() {
        let source = ChannelStreamSource {
            channel_id: Uuid::nil(),
            provider_pool_id: "provider".into(),
            source_id: "source".into(),
            generation: 1,
            upstream_url: "http://user:password@example.test/live?token=secret".into(),
            max_connections: 3,
            estimated_bitrate_bits_per_second: 8_000_000,
        };
        let debug = format!("{source:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("password"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn app_config_debug_redacts_all_plaintext_secrets() {
        let config = AppConfig {
            public_base_url: "https://gateway.test".to_owned(),
            output_token: "output-plaintext-secret".to_owned(),
            admin_bootstrap_token: "bootstrap-plaintext-secret".to_owned(),
            admin_password_hash: "password-hash-secret".to_owned(),
            master_key: MasterKey::from_bytes([9_u8; 32]),
            tuner_count: 3,
            runtime_versions: RuntimeVersions::default(),
            oidc: None,
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("https://gateway.test"));
        assert!(debug.contains("<redacted>"));
        for secret in ["output-plaintext", "bootstrap-plaintext", "password-hash"] {
            assert!(!debug.contains(secret));
        }
    }

    fn playback_candidate(
        stream_id: Uuid,
        pool_id: Uuid,
        max_connections: i32,
    ) -> ChannelPlaybackCandidateRow {
        ChannelPlaybackCandidateRow {
            channel_id: Uuid::from_u128(1),
            channel_revision: 4,
            provider_stream_id: stream_id,
            source_snapshot_id: Uuid::from_u128(stream_id.as_u128() + 100),
            provider_account_id: Uuid::from_u128(stream_id.as_u128() + 200),
            provider_revision: 2,
            provider_pool_id: pool_id,
            max_connections,
            input_adapter: "native-ts".to_owned(),
            stream_url: format!("http://provider.test/{stream_id}.ts"),
            url_secret_ciphertext: None,
            priority: 0,
            quality_rank: 0,
            health_status: "alive".to_owned(),
            bitrate_kbps: Some(8_000),
        }
    }

    #[test]
    fn database_source_spec_applies_pool_caps_alternates_and_stable_generation() {
        let app_state = state();
        let primary_pool = Uuid::from_u128(10);
        let alternate_pool = Uuid::from_u128(20);
        let first = playback_candidate(Uuid::from_u128(30), primary_pool, 3);
        let second = playback_candidate(Uuid::from_u128(40), alternate_pool, 2);
        let plan = ChannelPlaybackPlan {
            channel_id: first.channel_id,
            channel_revision: first.channel_revision,
            candidates: vec![first.clone(), second.clone()],
        };

        let generation = playback_generation(&plan);
        let spec = database_source_spec(&app_state, &plan).expect("database source spec");
        assert_eq!(spec.key().source_id.as_ref(), plan.channel_id.to_string());
        assert_eq!(spec.key().generation, generation);
        assert_eq!(spec.adapter_policy(), InputAdapterPolicy::NativeTs);
        assert!(format!("{spec:?}").contains("alternate_count: 1"));
        assert_eq!(
            app_state
                .media
                .provider_snapshot(&primary_pool.to_string())
                .unwrap()
                .capacity,
            3
        );
        assert_eq!(
            app_state
                .media
                .provider_snapshot(&alternate_pool.to_string())
                .unwrap()
                .capacity,
            2
        );

        let reordered = ChannelPlaybackPlan {
            candidates: vec![second, first.clone()],
            ..plan.clone()
        };
        assert_eq!(playback_generation(&reordered), generation);
        let mut changed = first;
        changed.provider_revision += 1;
        let changed = ChannelPlaybackPlan {
            candidates: vec![changed],
            ..plan
        };
        assert_ne!(playback_generation(&changed), generation);
    }

    #[tokio::test]
    async fn database_source_spec_uses_supported_input_adapters_and_redacts_invalid_values() {
        let app_state = state();
        for (input_adapter, expected) in [
            ("auto", InputAdapterPolicy::Auto),
            ("native-ts", InputAdapterPolicy::NativeTs),
            ("ffmpeg", InputAdapterPolicy::Ffmpeg),
            ("vlc", InputAdapterPolicy::Vlc),
        ] {
            let mut candidate = playback_candidate(Uuid::now_v7(), Uuid::now_v7(), 1);
            candidate.input_adapter = input_adapter.to_owned();
            let plan = ChannelPlaybackPlan {
                channel_id: candidate.channel_id,
                channel_revision: candidate.channel_revision,
                candidates: vec![candidate],
            };
            let spec = database_source_spec(&app_state, &plan).unwrap();
            assert_eq!(spec.adapter_policy(), expected, "{input_adapter}");
        }

        let mut candidate = playback_candidate(Uuid::now_v7(), Uuid::now_v7(), 1);
        candidate.input_adapter = "invalid-adapter?token=secret".to_owned();
        let plan = ChannelPlaybackPlan {
            channel_id: candidate.channel_id,
            channel_revision: candidate.channel_revision,
            candidates: vec![candidate],
        };
        let response = *database_source_spec(&app_state, &plan).unwrap_err();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let problem: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        assert_eq!(problem["code"], "invalid-input-adapter");
        assert!(!problem.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn hls_input_requires_a_process_adapter_without_disclosing_source_details() {
        let response =
            session_start_response(SessionStartError::ProcessInputRequiresProcessAdapter);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let problem: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        assert_eq!(problem["code"], "hls-process-adapter-required");
        assert_eq!(problem["title"], "HLS adapter configuration required");
        assert!(!problem.to_string().contains("token"));
        assert!(!problem.to_string().contains("password"));
    }

    #[test]
    fn database_source_spec_rejects_invalid_capacity_and_ciphertext() {
        let app_state = state();
        let mut candidate = playback_candidate(Uuid::from_u128(50), Uuid::from_u128(60), 0);
        let plan = ChannelPlaybackPlan {
            channel_id: candidate.channel_id,
            channel_revision: candidate.channel_revision,
            candidates: vec![candidate.clone()],
        };
        let response = database_source_spec(&app_state, &plan).unwrap_err();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        candidate.max_connections = 1;
        candidate.url_secret_ciphertext = Some(vec![1, 2, 3]);
        let plan = ChannelPlaybackPlan {
            candidates: vec![candidate],
            ..plan
        };
        let response = database_source_spec(&app_state, &plan).unwrap_err();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn default_ring_is_eight_seconds_with_a_hard_byte_ceiling() {
        let ordinary = default_ring(8_000_000).unwrap();
        assert!(ordinary.capacity_bytes() < 64 * 1024 * 1024);
        assert!(ordinary.pre_roll_packets() < ordinary.capacity_packets());

        let enormous = default_ring(u64::MAX).unwrap();
        assert!(enormous.capacity_bytes() <= 64 * 1024 * 1024);
        assert!(enormous.pre_roll_packets() <= enormous.capacity_packets());
    }

    #[tokio::test]
    async fn epg_mapping_endpoints_require_admin_auth() {
        let app_state = state();
        let app = router(app_state);
        for (path, method) in [
            ("/api/v1/epg/mappings", "GET"),
            ("/api/v1/epg/unmapped", "GET"),
            ("/api/v1/epg/channels/search?q=test", "GET"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn epg_reconcile_requires_admin_mutation() {
        let app_state = state();
        let app = router(app_state);
        let response = app
            .oneshot(
                Request::post("/api/v1/epg/reconcile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn epg_mapping_endments_return_persistence_unavailable_without_database() {
        let app_state = state();
        let app = router(app_state);
        let response = app
            .oneshot(
                Request::get("/api/v1/epg/mappings")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_source_routes_cover_crud_sync_and_validation() {
        let (admin, database, schema) = isolated_database().await;
        let app = router(state_with_database(Some(database.clone())));
        let invalid = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/sources",
                Some("{}".to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let created = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/sources",
                Some(
                    r#"{"name":"Route source","kind":"M3U","endpoint":"https://provider.test/list.m3u"}"#
                        .to_owned(),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: serde_json::Value =
            serde_json::from_str(&response_text(created).await).unwrap();
        let source_id = created["id"].as_str().unwrap();

        for (path, body, expected) in [
            (
                format!("/api/v1/sources/{source_id}/refresh-interval"),
                r#"{"refreshIntervalSeconds":-1}"#.to_owned(),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!("/api/v1/sources/{source_id}"),
                r#"{"maxConnections":0}"#.to_owned(),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(admin_request("PATCH", path, Some(body)))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let refreshed = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/sources/{source_id}/refresh-interval"),
                Some(r#"{"refreshIntervalSeconds":120}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(refreshed.status(), StatusCode::NO_CONTENT);
        let updated = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/sources/{source_id}"),
                Some(
                    r#"{"maxConnections":4,"timezone":"America/Denver","enabled":true}"#.to_owned(),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(updated.status(), StatusCode::NO_CONTENT);

        let status = app
            .clone()
            .oneshot(admin_request(
                "GET",
                format!("/api/v1/sources/{source_id}/sync-status"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let duplicate = app
            .clone()
            .oneshot(admin_request(
                "POST",
                format!("/api/v1/sources/{source_id}/sync"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        let cancelled = app
            .clone()
            .oneshot(admin_request(
                "POST",
                format!("/api/v1/sources/{source_id}/sync/cancel"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        let deleted = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                format!("/api/v1/sources/{source_id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        let missing = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                format!("/api/v1/sources/{source_id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        drop(app);
        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_catalog_configuration_routes_cover_crud_and_errors() {
        let (admin, database, schema) = isolated_database().await;
        let pool = database.pool().clone();
        let channel_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO channels (id, channel_number, name, group_name) VALUES ($1, '41', 'Database Channel', 'Database Group')",
        )
        .bind(channel_id)
        .execute(&pool)
        .await
        .unwrap();
        let app = router(state_with_database(Some(database.clone())));

        for path in [
            "/api/v1/channels?search=Database",
            "/api/v1/groups",
            "/api/v1/programmes",
            "/api/v1/streams/health",
            "/api/v1/streams/health/stats",
            "/api/v1/recordings/rules",
            "/api/v1/recordings",
            "/api/v1/recordings/stats",
        ] {
            let response = app
                .clone()
                .oneshot(admin_request("GET", path, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
        let disabled = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/channels/{channel_id}/enabled"),
                Some(r#"{"enabled":false}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(disabled.status(), StatusCode::OK);
        let group_enabled = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                "/api/v1/groups/Database%20Group/enabled",
                Some(r#"{"enabled":true}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(group_enabled.status(), StatusCode::OK);
        let all_enabled = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                "/api/v1/groups/enabled",
                Some(r#"{"enabled":false}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(all_enabled.status(), StatusCode::OK);

        let invalid_role = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/users",
                Some(r#"{"username":"route-user","display_name":"Route user","password":"password","role":"invalid"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(invalid_role.status(), StatusCode::BAD_REQUEST);
        let user = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/users",
                Some(r#"{"username":"route-user","display_name":"Route user","password":"password","role":"viewer"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(user.status(), StatusCode::CREATED);
        let user: serde_json::Value = serde_json::from_str(&response_text(user).await).unwrap();
        let user_id = user["id"].as_str().unwrap();
        let updated_user = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/users/{user_id}"),
                Some(
                    r#"{"display_name":"Updated user","role":"operator","enabled":false}"#
                        .to_owned(),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(updated_user.status(), StatusCode::OK);
        let deleted_user = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                format!("/api/v1/users/{user_id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(deleted_user.status(), StatusCode::NO_CONTENT);

        let alias = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/channel-aliases",
                Some(r#"{"canonical_name":"Route Channel","alias":"Route Channel HD","country":"US","category":"test"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(alias.status(), StatusCode::CREATED);
        let alias: serde_json::Value = serde_json::from_str(&response_text(alias).await).unwrap();
        let alias_id = alias["id"].as_str().unwrap();
        let resolved = app
            .clone()
            .oneshot(admin_request(
                "GET",
                "/api/v1/channel-aliases/resolve?name=Route%20Channel%20HD",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resolved.status(), StatusCode::OK);
        let deleted_alias = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                format!("/api/v1/channel-aliases/{alias_id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(deleted_alias.status(), StatusCode::NO_CONTENT);

        let bad_recording_rule = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/recordings/rules",
                Some(r#"{"name":"Bad","channel_id":"invalid","rule_type":"wrong"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(bad_recording_rule.status(), StatusCode::BAD_REQUEST);
        let rule = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/recordings/rules",
                Some(format!(r#"{{"name":"Route rule","channel_id":"{channel_id}","rule_type":"series","keep_until":"forever"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(rule.status(), StatusCode::CREATED);
        let rule: serde_json::Value = serde_json::from_str(&response_text(rule).await).unwrap();
        let rule_id = rule["id"].as_str().unwrap();
        let recording = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/recordings",
                Some(format!(r#"{{"rule_id":"{rule_id}","channel_id":"{channel_id}","title":"Route recording","description":"test","starts_at":"2026-01-01T00:00:00Z","ends_at":"2026-01-01T01:00:00Z"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(recording.status(), StatusCode::CREATED);
        let recording: serde_json::Value =
            serde_json::from_str(&response_text(recording).await).unwrap();
        let recording_id = recording["id"].as_str().unwrap();
        for (path, expected) in [
            (
                format!("/api/v1/recordings/{recording_id}"),
                StatusCode::NO_CONTENT,
            ),
            (
                format!("/api/v1/recordings/rules/{rule_id}"),
                StatusCode::NO_CONTENT,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(admin_request("DELETE", path, None))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }

        drop(app);
        drop(pool);
        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_template_and_stream_profile_routes_cover_crud() {
        let (admin, database, schema) = isolated_database().await;
        let pool = database.pool().clone();
        let channel_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO channels (id, channel_number, name) VALUES ($1, '52', 'Template Channel')",
        )
        .bind(channel_id)
        .execute(&pool)
        .await
        .unwrap();
        let app = router(state_with_database(Some(database.clone())));

        let template_body = r#"{"name":"Route events","displayName":"Route Events","matchRegex":"Route.*","channelNameFormat":"Route {slot}","groupName":"Events","eventDurationHours":3,"pastDateGraceHours":4,"futureDateDays":2}"#;
        let template = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/event-templates",
                Some(template_body.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(template.status(), StatusCode::CREATED);
        let template: serde_json::Value =
            serde_json::from_str(&response_text(template).await).unwrap();
        let template_id = template["id"].as_str().unwrap();
        for (method, path, body, expected) in [
            (
                "GET",
                "/api/v1/event-templates".to_owned(),
                None,
                StatusCode::OK,
            ),
            (
                "POST",
                format!("/api/v1/event-templates/{template_id}/scan"),
                None,
                StatusCode::OK,
            ),
            (
                "GET",
                format!("/api/v1/event-channels?templateId={template_id}"),
                None,
                StatusCode::OK,
            ),
            (
                "POST",
                format!("/api/v1/event-templates/{template_id}/prune"),
                None,
                StatusCode::OK,
            ),
            (
                "PATCH",
                format!("/api/v1/event-templates/{template_id}"),
                Some(template_body.to_owned()),
                StatusCode::OK,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(admin_request(method, path, body))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }

        let lineup = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/lineup-templates",
                Some(r#"{"name":"Route lineup","packageName":"Route package","country":"US","description":"test","categories":[{"name":"Route category","sortOrder":0,"channels":[{"name":"Route lineup channel","channelNumber":"52","aliases":["Route"]}]}]}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(lineup.status(), StatusCode::CREATED);
        let lineup: serde_json::Value = serde_json::from_str(&response_text(lineup).await).unwrap();
        let lineup_id = lineup["id"].as_str().unwrap();
        for (method, path, expected) in [
            ("GET", "/api/v1/lineup-templates".to_owned(), StatusCode::OK),
            (
                "GET",
                format!("/api/v1/lineup-templates/{lineup_id}/categories"),
                StatusCode::OK,
            ),
            (
                "GET",
                format!("/api/v1/lineup-templates/{lineup_id}/channels"),
                StatusCode::OK,
            ),
            (
                "POST",
                format!("/api/v1/lineup-templates/{lineup_id}/apply"),
                StatusCode::OK,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(admin_request(method, path, None))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }

        let invalid_profile = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/stream-profiles",
                Some(r#"{"name":"Bad profile","profile_type":"bad"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(invalid_profile.status(), StatusCode::BAD_REQUEST);
        let profile = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/stream-profiles",
                Some(r#"{"name":"Route profile","profile_type":"ffmpeg","command":"ffmpeg","arguments":["-i"],"buffer_seconds":2.5,"user_agent":"Route"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(profile.status(), StatusCode::CREATED);
        let profile: serde_json::Value =
            serde_json::from_str(&response_text(profile).await).unwrap();
        let profile_id = profile["id"].as_str().unwrap();
        let assigned = app
            .clone()
            .oneshot(admin_request(
                "POST",
                format!("/api/v1/channels/{channel_id}/stream-profile"),
                Some(format!(r#"{{"stream_profile_id":"{profile_id}"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(assigned.status(), StatusCode::NO_CONTENT);
        let removed = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                format!("/api/v1/channels/{channel_id}/stream-profile"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);
        for path in [
            "/api/v1/stream-profiles".to_owned(),
            format!("/api/v1/event-templates/{template_id}"),
            format!("/api/v1/lineup-templates/{lineup_id}"),
            format!("/api/v1/stream-profiles/{profile_id}"),
        ] {
            let method = if path == "/api/v1/stream-profiles" {
                "GET"
            } else {
                "DELETE"
            };
            let response = app
                .clone()
                .oneshot(admin_request(method, path, None))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if method == "GET" {
                    StatusCode::OK
                } else {
                    StatusCode::NO_CONTENT
                }
            );
        }

        drop(app);
        drop(pool);
        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_event_template_patch_applies_partial_updates() {
        let (admin, database, schema) = isolated_database().await;
        let pool = database.pool().clone();
        let app = router(state_with_database(Some(database.clone())));

        let template_body = r#"{"name":"Patch events","displayName":"Patch Events","matchRegex":"Patch.*","channelNameFormat":"Patch {slot}","groupName":"Events","eventDurationHours":3,"pastDateGraceHours":4,"futureDateDays":2,"timezone":"America/Denver","fillerTitle":"Off air"}"#;
        let created = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/v1/event-templates",
                Some(template_body.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let template: serde_json::Value =
            serde_json::from_str(&response_text(created).await).unwrap();
        let template_id = template["id"].as_str().unwrap();
        assert_eq!(template["enabled"], serde_json::Value::Bool(true));
        assert_eq!(template["eventDurationHours"], serde_json::json!(3));
        assert_eq!(template["timezone"], serde_json::json!("America/Denver"));
        assert_eq!(template["fillerTitle"], serde_json::json!("Off air"));

        // Partial patch: toggle enabled off and adjust duration only.
        let disabled = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/event-templates/{template_id}"),
                Some(r#"{"enabled":false,"eventDurationHours":5}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(disabled.status(), StatusCode::OK);
        let disabled_body: serde_json::Value =
            serde_json::from_str(&response_text(disabled).await).unwrap();
        assert_eq!(disabled_body["enabled"], serde_json::Value::Bool(false));
        assert_eq!(disabled_body["eventDurationHours"], serde_json::json!(5));
        // Omitted fields keep their stored values.
        assert_eq!(
            disabled_body["displayName"],
            serde_json::json!("Patch Events")
        );
        assert_eq!(disabled_body["futureDateDays"], serde_json::json!(2));
        assert_eq!(
            disabled_body["timezone"],
            serde_json::json!("America/Denver")
        );
        assert_eq!(disabled_body["fillerTitle"], serde_json::json!("Off air"));

        // Empty patch still succeeds and refreshes updated_at.
        let empty = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/event-templates/{template_id}"),
                Some("{}".to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::OK);

        // Re-enable and rename in one partial patch.
        let reenabled = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/event-templates/{template_id}"),
                Some(r#"{"enabled":true,"displayName":"Renamed Events"}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(reenabled.status(), StatusCode::OK);
        let reenabled_body: serde_json::Value =
            serde_json::from_str(&response_text(reenabled).await).unwrap();
        assert_eq!(reenabled_body["enabled"], serde_json::Value::Bool(true));
        assert_eq!(
            reenabled_body["displayName"],
            serde_json::json!("Renamed Events")
        );

        // Unknown template ID returns 404.
        let missing = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                format!("/api/v1/event-templates/{}", Uuid::now_v7()),
                Some(r#"{"enabled":false}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        // Invalid UUID returns 400.
        let bad_id = app
            .clone()
            .oneshot(admin_request(
                "PATCH",
                "/api/v1/event-templates/not-a-uuid",
                Some(r#"{"enabled":false}"#.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(bad_id.status(), StatusCode::BAD_REQUEST);

        // Verify persistence: the enabled flag and duration are stored.
        let stored: iptv_persistence::EventTemplateRow =
            sqlx::query_as::<_, iptv_persistence::EventTemplateRow>(
                r"SELECT id, name, display_name, match_regex, channel_name_format,
                          group_name, event_duration_hours, past_date_grace_hours,
                          future_date_days, timezone, filler_title, enabled
                   FROM event_templates WHERE id = $1",
            )
            .bind(Uuid::parse_str(template_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(stored.enabled);
        assert_eq!(stored.event_duration_hours, 5);
        assert_eq!(stored.display_name, "Renamed Events");
        assert_eq!(stored.timezone, "America/Denver");
        assert_eq!(stored.filler_title, "Off air");

        drop(app);
        drop(pool);
        drop_isolated_database(&admin, database, schema).await;
    }

    #[tokio::test]
    async fn durable_route_errors_are_consistent_without_postgres() {
        let app = router(state());
        let source_id = Uuid::nil();
        let job_id = Uuid::nil();
        for (method, path, body) in [
            ("GET", "/api/v1/sources".to_owned(), None),
            ("DELETE", format!("/api/v1/sources/{source_id}"), None),
            (
                "PATCH",
                format!("/api/v1/sources/{source_id}/refresh-interval"),
                Some(r#"{"refreshIntervalSeconds":10}"#.to_owned()),
            ),
            ("POST", format!("/api/v1/sources/{source_id}/sync"), None),
            (
                "GET",
                format!("/api/v1/sources/{source_id}/sync-status"),
                None,
            ),
            (
                "POST",
                format!("/api/v1/sources/{source_id}/sync/cancel"),
                None,
            ),
            ("GET", "/api/v1/groups".to_owned(), None),
            ("GET", "/api/v1/jobs".to_owned(), None),
            ("POST", format!("/api/v1/jobs/{job_id}/cancel"), None),
            ("POST", "/api/v1/epg/reconcile".to_owned(), None),
            (
                "GET",
                format!("/api/v1/sources/{source_id}/reconcile/revisions"),
                None,
            ),
            (
                "POST",
                format!("/api/v1/sources/{source_id}/reconcile/rollback"),
                Some(r#"{"revision":1}"#.to_owned()),
            ),
            ("GET", "/api/v1/epg/unmapped".to_owned(), None),
            ("GET", "/api/v1/epg/channels/search?q=test".to_owned(), None),
            ("GET", "/api/v1/event-templates".to_owned(), None),
            ("GET", "/api/v1/event-channels".to_owned(), None),
            ("GET", "/api/v1/lineup-templates".to_owned(), None),
            ("GET", "/api/v1/streams/health".to_owned(), None),
            ("GET", "/api/v1/streams/health/stats".to_owned(), None),
            ("POST", "/api/v1/streams/health/check".to_owned(), None),
            ("POST", "/api/v1/streams/rank".to_owned(), None),
            ("GET", "/api/v1/users".to_owned(), None),
            ("GET", "/api/v1/channel-aliases".to_owned(), None),
            ("GET", "/api/v1/recordings/rules".to_owned(), None),
            ("GET", "/api/v1/recordings".to_owned(), None),
            ("GET", "/api/v1/recordings/stats".to_owned(), None),
            ("GET", "/api/v1/stream-profiles".to_owned(), None),
        ] {
            let response = app
                .clone()
                .oneshot(admin_request(method, path, body))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method}"
            );
        }
    }

    #[test]
    fn generated_openapi_schemas_are_callable_for_every_api_type() {
        assert_schema::<RuntimeVersions>();
        assert_schema::<ChannelRecord>();
        assert_schema::<CreateChannelRequest>();
        assert_schema::<SystemInfo>();
        assert_schema::<CreateSourceRequest>();
        assert_schema::<UpdateSourceRequest>();
        assert_schema::<SourceResponse>();
        assert_schema::<JobResponse>();
        assert_schema::<ChannelResponse>();
        assert_schema::<ProgrammeResponse>();
        assert_schema::<ChannelPageResponse>();
        assert_schema::<GroupResponse>();
        assert_schema::<ProgrammePageResponse>();
        assert_schema::<PageQuery>();
        assert_schema::<DynamicEventResponse>();
        assert_schema::<EventTemplateResponse>();
        assert_schema::<CreateEventTemplateRequest>();
        assert_schema::<UpdateEventTemplateRequest>();
        assert_schema::<EventChannelResponse>();
        assert_schema::<EventChannelQuery>();
        assert_schema::<SessionResponse>();
        assert_schema::<JellyfinSetup>();
        assert_schema::<RotateJellyfinTokenRequest>();
        assert_schema::<SaveResult>();
        assert_schema::<LoginRequest>();
        assert_schema::<LogoutRequest>();
        assert_schema::<AuthUser>();
        assert_schema::<AuthStatus>();
        assert_schema::<ProblemDetails>();
        assert_schema::<UpdateRefreshIntervalRequest>();
        assert_schema::<SourceSyncResponse>();
        assert_schema::<SourceSyncStatusResponse>();
        assert_schema::<ChannelPreviewResponse>();
        assert_schema::<SetEnabledRequest>();
        assert_schema::<EpgMappingResponse>();
        assert_schema::<EpgMappingPageResponse>();
        assert_schema::<UnmappedChannelResponse>();
        assert_schema::<UnmappedChannelPageResponse>();
        assert_schema::<ReviewCandidateResponse>();
        assert_schema::<EpgChannelSearchResponse>();
        assert_schema::<EpgReconcileResponse>();
        assert_schema::<SetEpgMappingRequest>();
        assert_schema::<ResolveReviewRequest>();
        assert_schema::<CatalogEvent>();
        assert_schema::<LineupTemplateResponse>();
        assert_schema::<LineupCategoryResponse>();
        assert_schema::<LineupChannelResponse>();
        assert_schema::<LineupApplyStatsResponse>();
        assert_schema::<LineupChannelInput>();
        assert_schema::<LineupCategoryInput>();
        assert_schema::<CreateLineupTemplateRequest>();
        assert_schema::<StreamHealthItem>();
        assert_schema::<StreamHealthResponse>();
        assert_schema::<StreamHealthStatsResponse>();
        assert_schema::<BestStreamResponse>();
        assert_schema::<TriggerHealthCheckRequest>();
        assert_schema::<HealthCheckTriggerResponse>();
        assert_schema::<StreamRankResponse>();
        assert_schema::<UserResponse>();
        assert_schema::<CreateUserRequest>();
        assert_schema::<UpdateUserRequest>();
        assert_schema::<ChannelAliasResponse>();
        assert_schema::<ChannelAliasPageResponse>();
        assert_schema::<CreateChannelAliasRequest>();
        assert_schema::<ResolveAliasResponse>();
        assert_schema::<RecordingRuleResponse>();
        assert_schema::<CreateRecordingRuleRequest>();
        assert_schema::<RecordingResponse>();
        assert_schema::<CreateRecordingRequest>();
        assert_schema::<RecordingPageResponse>();
        assert_schema::<RecordingStatsResponse>();
        assert_schema::<StreamProfileResponse>();
        assert_schema::<CreateStreamProfileRequest>();
        assert_schema::<AssignStreamProfileRequest>();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn api_response_models_serialize_all_optional_and_populated_fields() {
        let now = Utc::now();
        let id = Uuid::now_v7().to_string();
        let channel = ChannelResponse {
            id: id.clone(),
            number: "1".to_owned(),
            name: "Channel".to_owned(),
            group: "Group".to_owned(),
            tvg_id: "tvg".to_owned(),
            streams: 1,
            primary_codec: "Stream copy".to_owned(),
            bitrate_kbps: 8_000,
            state: "healthy".to_owned(),
            enabled: true,
        };
        let programme = ProgrammeResponse {
            id: id.clone(),
            channel: "Channel".to_owned(),
            title: "Programme".to_owned(),
            start: now,
            end: now,
            source: "Source".to_owned(),
            confidence: 100,
        };
        assert_serializes(RuntimeVersions {
            gateway: "test".to_owned(),
            ffmpeg: Some("ffmpeg".to_owned()),
            vlc: Some("vlc".to_owned()),
        });
        assert_serializes(ChannelRecord {
            id: Uuid::now_v7(),
            number: "1".to_owned(),
            name: "Channel".to_owned(),
            group: Some("Group".to_owned()),
            logo_url: Some("https://gateway.test/logo.png".to_owned()),
            enabled: true,
        });
        assert_serializes(SystemInfo {
            channels: 1,
            healthy_streams: 1,
            active_sessions: 1,
            guide_coverage: 100.0,
            provider_connections: 1,
            provider_limit: 3,
            uptime_seconds: 1,
            versions: RuntimeVersions::default(),
        });
        assert_serializes(SourceResponse {
            id: id.clone(),
            name: "Source".to_owned(),
            kind: "M3U".to_owned(),
            state: "ready".to_owned(),
            channels: 1,
            last_sync: now,
            endpoint: "https://provider.test/list.m3u".to_owned(),
            refresh_interval_seconds: 60,
            last_refreshed_at: Some(now),
            max_connections: 2,
            timezone: "UTC".to_owned(),
            enabled: true,
        });
        assert_serializes(JobResponse {
            id: id.clone(),
            kind: "refresh-source".to_owned(),
            status: "queued".to_owned(),
            progress: serde_json::json!({"percent": 50}),
            attempts: 1,
            max_attempts: 3,
            last_error: Some("none".to_owned()),
            created_at: now,
            updated_at: now,
            completed_at: Some(now),
        });
        assert_serializes(channel.clone());
        assert_serializes(programme.clone());
        assert_serializes(ChannelPageResponse {
            total: 1,
            limit: 100,
            offset: 0,
            items: vec![channel],
        });
        assert_serializes(GroupResponse {
            name: "Group".to_owned(),
            channel_count: 1,
            enabled_count: 1,
        });
        assert_serializes(ProgrammePageResponse {
            total: 1,
            limit: 100,
            offset: 0,
            items: vec![programme],
        });
        assert_serializes(DynamicEventResponse {
            id: id.clone(),
            group: "Events".to_owned(),
            raw_title: "Raw".to_owned(),
            programme_title: "Programme".to_owned(),
            channel_slot: "1".to_owned(),
            start: now,
            state: "scheduled".to_owned(),
            template: "Template".to_owned(),
        });
        assert_serializes(EventTemplateResponse {
            id: id.clone(),
            name: "template".to_owned(),
            display_name: "Template".to_owned(),
            match_regex: ".*".to_owned(),
            channel_name_format: "Event {slot}".to_owned(),
            group_name: "Events".to_owned(),
            event_duration_hours: 3,
            past_date_grace_hours: 4,
            future_date_days: 2,
            timezone: "UTC".to_owned(),
            filler_title: "No programs available".to_owned(),
            enabled: true,
        });
        assert_serializes(EventChannelResponse {
            id: id.clone(),
            template_id: id.clone(),
            channel_id: Some(id.clone()),
            slot_number: 1,
            event_title: Some("Event".to_owned()),
            event_start: Some(now),
            event_end: Some(now),
            raw_stream_name: Some("Raw".to_owned()),
            state: "scheduled".to_owned(),
        });
        assert_serializes(SessionResponse {
            provider_pool_id: "pool".to_owned(),
            source_id: "source".to_owned(),
            configured_generation: 1,
            upstream_generation: 1,
            state: "streaming".to_owned(),
            viewer_count: 1,
            retained_packets: 1,
            capacity_packets: 2,
            lag_events: 0,
            wrap_events: 0,
            overwritten_packets: 0,
            reconnect_attempts: 0,
            failover_attempts: 0,
            failure_count: 0,
            last_failure: Some("http".to_owned()),
            provider_capacity: 3,
            provider_active_sessions: 1,
            provider_high_watermark: 1,
            provider_available_slots: 2,
        });
        assert_serializes(SaveResult {
            ok: true,
            message: "Saved".to_owned(),
        });
        assert_serializes(AuthStatus {
            authenticated: true,
            user: Some(AuthUser {
                id: id.clone(),
                username: "operator".to_owned(),
                display_name: "Operator".to_owned(),
            }),
            oidc_enabled: None,
        });
        assert_serializes(ProblemDetails::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            "Invalid",
            "A detail",
        ));
        assert_serializes(SourceSyncResponse {
            job_id: id.clone(),
            message: "Queued".to_owned(),
        });
        assert_serializes(SourceSyncStatusResponse {
            job_id: id.clone(),
            status: "running".to_owned(),
            stage: "download".to_owned(),
            percent: 50,
            message: "Half done".to_owned(),
            bytes_downloaded: 10,
            records_processed: 2,
            started_at: now,
            updated_at: now,
        });
        assert_serializes(ChannelPreviewResponse {
            stream_url: "/api/v1/channels/id/stream".to_owned(),
            content_type: "video/mp2t".to_owned(),
        });
        let mapping = EpgMappingResponse {
            channel_id: id.clone(),
            epg_channel_id: id.clone(),
            method: "exact-name".to_owned(),
            confidence: 1.0,
            evidence: serde_json::json!({"match": "exact"}),
            review_status: "accepted".to_owned(),
            reviewed_by: Some("operator".to_owned()),
            reviewed_at: Some(now),
            revision: 1,
            updated_at: now,
            channel_name: "Channel".to_owned(),
            canonical_key: Some("channel".to_owned()),
            epg_xmltv_id: Some("channel.test".to_owned()),
            epg_display_name: Some("Channel".to_owned()),
        };
        assert_serializes(mapping.clone());
        assert_serializes(EpgMappingPageResponse {
            total: 1,
            limit: 100,
            offset: 0,
            items: vec![mapping],
        });
        let unmapped = UnmappedChannelResponse {
            id: id.clone(),
            name: "Unmapped".to_owned(),
            canonical_key: Some("unmapped".to_owned()),
            group_name: Some("Group".to_owned()),
        };
        assert_serializes(unmapped.clone());
        assert_serializes(UnmappedChannelPageResponse {
            total: 1,
            limit: 100,
            offset: 0,
            items: vec![unmapped],
        });
        assert_serializes(ReviewCandidateResponse {
            id: id.clone(),
            channel_id: id.clone(),
            epg_channel_id: id.clone(),
            method: "fuzzy".to_owned(),
            confidence: 0.8,
            evidence: serde_json::json!({}),
            created_at: now,
            epg_xmltv_id: Some("id".to_owned()),
            epg_display_name: Some("Channel".to_owned()),
        });
        assert_serializes(EpgChannelSearchResponse {
            id: id.clone(),
            xmltv_id: "channel.test".to_owned(),
            display_name: Some("Channel".to_owned()),
        });
        assert_serializes(EpgReconcileResponse {
            mappings_applied: 1,
            mappings_removed: 1,
            review_queued: 1,
        });
        assert_serializes(CatalogEvent {
            event: "heartbeat",
            timestamp: now,
        });
        assert_serializes(LineupTemplateResponse {
            id: id.clone(),
            name: "Lineup".to_owned(),
            package_name: "Package".to_owned(),
            country: "US".to_owned(),
            description: Some("Description".to_owned()),
            enabled: true,
        });
        assert_serializes(LineupCategoryResponse {
            id: id.clone(),
            template_id: id.clone(),
            name: "Category".to_owned(),
            sort_order: 0,
        });
        assert_serializes(LineupChannelResponse {
            id: id.clone(),
            category_id: id.clone(),
            name: "Channel".to_owned(),
            channel_number: "1".to_owned(),
            aliases: vec!["Alias".to_owned()],
            enabled: true,
        });
        assert_serializes(LineupApplyStatsResponse {
            matched: 1,
            unmatched: 1,
            enabled: 1,
            disabled: 1,
        });
        let health = StreamHealthItem {
            provider_stream_id: id.clone(),
            stream_name: "Stream".to_owned(),
            group_name: Some("Group".to_owned()),
            health_status: "alive".to_owned(),
            health_checked_at: Some(now.to_rfc3339()),
            health_error: Some("none".to_owned()),
            video_codec: Some("h264".to_owned()),
            video_resolution: Some("1920x1080".to_owned()),
            video_width: Some(1920),
            video_height: Some(1080),
            video_fps: Some(30.0),
            audio_codec: Some("aac".to_owned()),
            audio_channels: Some(2),
            audio_sample_rate: Some(48_000),
            bitrate_kbps: Some(8_000),
            provider_account_id: id.clone(),
        };
        assert_serializes(health);
        assert_serializes(StreamHealthResponse {
            total: 0,
            items: vec![],
            estimated: false,
        });
        assert_serializes(StreamHealthStatsResponse {
            alive: 1,
            dead: 1,
            unknown: 1,
            checking: 1,
        });
        assert_serializes(BestStreamResponse {
            channel_id: id.clone(),
            channel_name: "Channel".to_owned(),
            provider_stream_id: id.clone(),
            stream_name: "Stream".to_owned(),
            priority: 0,
            quality_rank: 0,
            health_status: "alive".to_owned(),
            video_width: Some(1920),
            video_height: Some(1080),
            video_fps: Some(30.0),
            video_codec: Some("h264".to_owned()),
            failover_count: 0,
            last_failover_at: Some(now.to_rfc3339()),
            url_template: "https://provider.test/stream".to_owned(),
            provider_account_id: id.clone(),
        });
        assert_serializes(HealthCheckTriggerResponse { queued: 1 });
        assert_serializes(StreamRankResponse { ranked: 1 });
        assert_serializes(UserResponse {
            id: id.clone(),
            username: "operator".to_owned(),
            display_name: "Operator".to_owned(),
            role: "operator".to_owned(),
            enabled: true,
            last_login_at: Some(now.to_rfc3339()),
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        });
        let alias = ChannelAliasResponse {
            id: id.clone(),
            canonical_name: "Channel".to_owned(),
            alias: "Channel HD".to_owned(),
            country: Some("US".to_owned()),
            category: Some("News".to_owned()),
            created_at: now.to_rfc3339(),
        };
        assert_serializes(ChannelAliasPageResponse {
            total: 1,
            items: vec![alias],
        });
        assert_serializes(ResolveAliasResponse {
            canonical_name: Some("Channel".to_owned()),
            input: "Channel HD".to_owned(),
        });
        assert_serializes(RecordingRuleResponse {
            id: id.clone(),
            name: "Rule".to_owned(),
            channel_id: id.clone(),
            rule_type: "series".to_owned(),
            title_filter: Some("Title".to_owned()),
            category_filter: Some("News".to_owned()),
            start_padding_minutes: 1,
            end_padding_minutes: 1,
            max_recordings: Some(5),
            keep_until: "forever".to_owned(),
            enabled: true,
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        });
        let recording = RecordingResponse {
            id: id.clone(),
            rule_id: Some(id.clone()),
            channel_id: id.clone(),
            programme_id: Some(id.clone()),
            title: "Recording".to_owned(),
            description: Some("Description".to_owned()),
            starts_at: now.to_rfc3339(),
            ends_at: now.to_rfc3339(),
            status: "scheduled".to_owned(),
            file_path: Some("/recordings/test.ts".to_owned()),
            file_size_bytes: Some(1),
            duration_seconds: Some(1),
            error_message: Some("none".to_owned()),
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        };
        assert_serializes(RecordingPageResponse {
            total: 1,
            items: vec![recording],
        });
        assert_serializes(RecordingStatsResponse {
            scheduled: 1,
            recording: 1,
            completed: 1,
            failed: 1,
            total_bytes: 1,
        });
        assert_serializes(StreamProfileResponse {
            id,
            name: "Profile".to_owned(),
            profile_type: "direct".to_owned(),
            command: Some("ffmpeg".to_owned()),
            arguments: serde_json::json!(["-i"]),
            buffer_seconds: 1.0,
            user_agent: Some("Agent".to_owned()),
            referer: Some("https://gateway.test".to_owned()),
            enabled: true,
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn api_request_models_deserialize_all_documented_fields() {
        let id = Uuid::now_v7().to_string();
        assert_deserializes::<CreateChannelRequest>(serde_json::json!({
            "number": "1", "name": "Channel", "group": "Group", "logo_url": "https://gateway.test/logo.png"
        }));
        assert_deserializes::<CreateSourceRequest>(serde_json::json!({
            "name": "Source", "kind": "M3U", "endpoint": "https://provider.test/list.m3u",
            "timezone": "America/Denver"
        }));
        assert_deserializes::<UpdateSourceRequest>(serde_json::json!({
            "maxConnections": 2, "timezone": "America/Denver", "enabled": true
        }));
        assert_deserializes::<UpdateRefreshIntervalRequest>(serde_json::json!({
            "refreshIntervalSeconds": 60
        }));
        assert_deserializes::<PageQuery>(serde_json::json!({
            "search": "channel", "group": "Group", "enabled": true, "channelId": id,
            "limit": 100, "offset": 0
        }));
        assert_deserializes::<CreateEventTemplateRequest>(serde_json::json!({
            "name": "template", "displayName": "Template", "matchRegex": ".*",
            "channelNameFormat": "Event {slot}", "groupName": "Events",
            "eventDurationHours": 3, "pastDateGraceHours": 4, "futureDateDays": 2
        }));
        assert_deserializes::<UpdateEventTemplateRequest>(serde_json::json!({
            "enabled": false
        }));
        assert_deserializes::<UpdateEventTemplateRequest>(serde_json::json!({
            "displayName": "Updated", "eventDurationHours": 4, "enabled": true
        }));
        assert_deserializes::<UpdateEventTemplateRequest>(serde_json::json!({}));
        assert_deserializes::<EventChannelQuery>(serde_json::json!({"templateId": id}));
        assert_deserializes::<LoginRequest>(serde_json::json!({
            "username": "operator", "password": "password"
        }));
        assert_deserializes::<LogoutRequest>(serde_json::json!({}));
        assert_deserializes::<SetEnabledRequest>(serde_json::json!({"enabled": true}));
        assert_deserializes::<SetEpgMappingRequest>(serde_json::json!({"epgChannelId": id}));
        assert_deserializes::<ResolveReviewRequest>(serde_json::json!({
            "accept": true, "epgChannelId": id
        }));
        assert_deserializes::<LineupChannelInput>(serde_json::json!({
            "name": "Channel", "channelNumber": "1", "aliases": ["Alias"]
        }));
        assert_deserializes::<LineupCategoryInput>(serde_json::json!({
            "name": "Category", "sortOrder": 1,
            "channels": [{"name": "Channel", "channelNumber": "1", "aliases": ["Alias"]}]
        }));
        assert_deserializes::<CreateLineupTemplateRequest>(serde_json::json!({
            "name": "Lineup", "packageName": "Package", "country": "US", "description": "Description",
            "categories": [{"name": "Category", "sortOrder": 1, "channels": []}]
        }));
        assert_deserializes::<TriggerHealthCheckRequest>(serde_json::json!({"limit": 50}));
        assert_deserializes::<CreateUserRequest>(serde_json::json!({
            "username": "operator", "display_name": "Operator", "password": "password", "role": "operator"
        }));
        assert_deserializes::<UpdateUserRequest>(serde_json::json!({
            "display_name": "Operator", "password": "password", "role": "admin", "enabled": true
        }));
        assert_deserializes::<CreateChannelAliasRequest>(serde_json::json!({
            "canonical_name": "Channel", "alias": "Channel HD", "country": "US", "category": "News"
        }));
        assert_deserializes::<CreateRecordingRuleRequest>(serde_json::json!({
            "name": "Rule", "channel_id": id, "rule_type": "series", "title_filter": "Title",
            "category_filter": "News", "start_padding_minutes": 1, "end_padding_minutes": 1,
            "max_recordings": 5, "keep_until": "forever"
        }));
        assert_deserializes::<CreateRecordingRequest>(serde_json::json!({
            "rule_id": id, "channel_id": id, "programme_id": id, "title": "Recording",
            "description": "Description", "starts_at": "2026-01-01T00:00:00Z",
            "ends_at": "2026-01-01T01:00:00Z"
        }));
        assert_deserializes::<CreateStreamProfileRequest>(serde_json::json!({
            "name": "Profile", "profile_type": "ffmpeg", "command": "ffmpeg", "arguments": ["-i"],
            "buffer_seconds": 1.0, "user_agent": "Agent", "referer": "https://gateway.test"
        }));
        assert_deserializes::<AssignStreamProfileRequest>(
            serde_json::json!({"stream_profile_id": id}),
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn in_memory_output_discovery_and_catalog_routes_return_contracts() {
        let app_state = state();
        let channel_id = Uuid::now_v7();
        {
            let catalog_handle = app_state.catalog();
            let mut catalog = catalog_handle.write().await;
            catalog.channels.push(ChannelRecord {
                id: channel_id,
                number: "8".to_owned(),
                name: "Discovery Channel".to_owned(),
                group: Some("Discovery".to_owned()),
                logo_url: Some("https://gateway.test/logo.png".to_owned()),
                enabled: true,
            });
            catalog.programmes.push(ProgrammeRecord {
                channel_id,
                starts_at: Utc::now(),
                stops_at: Utc::now() + chrono::Duration::hours(1),
                title: "Discovery programme".to_owned(),
                description: Some("A test programme".to_owned()),
                categories: vec!["Documentary".to_owned()],
            });
        }
        let app = router(app_state);
        let ready = app
            .clone()
            .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
        let openapi = app
            .clone()
            .oneshot(
                Request::get("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(openapi.status(), StatusCode::OK);

        let playlist = output_response(&app, "/out/output-secret/playlist.m3u").await;
        assert_eq!(playlist.status(), StatusCode::OK);
        assert_no_store(&playlist);
        assert!(response_text(playlist).await.contains("Discovery Channel"));

        let xmltv = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/xmltv.xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(xmltv.status(), StatusCode::OK);
        assert_no_store(&xmltv);
        assert!(response_text(xmltv).await.contains("Discovery programme"));
        let discover = output_response(&app, "/out/output-secret/hdhr/discover.json").await;
        assert_eq!(discover.status(), StatusCode::OK);
        assert_no_store(&discover);
        let lineup = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/lineup.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(lineup.status(), StatusCode::OK);
        assert_no_store(&lineup);
        assert!(response_text(lineup).await.contains("Discovery Channel"));
        let status = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/lineup_status.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        assert_no_store(&status);
        let device = app
            .clone()
            .oneshot(
                Request::get("/out/output-secret/hdhr/device.xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(device.status(), StatusCode::OK);
        assert_no_store(&device);
        assert!(response_text(device).await.contains("HDHR-IPTV"));

        let events = app
            .oneshot(
                Request::get("/api/v1/catalog-events")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events.status(), StatusCode::OK);
        let frame = tokio::time::timeout(Duration::from_secs(1), events.into_body().frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8(frame.into_data().unwrap().to_vec())
                .unwrap()
                .starts_with("event: heartbeat\n")
        );
    }
}
