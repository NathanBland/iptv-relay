//! HTTP control and Jellyfin-facing output APIs.

mod auth;

pub use auth::hash_admin_password;

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    fmt::{self, Write as _},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use chrono::{DateTime, Utc};
use iptv_domain::{SettingDefinition, setting_catalog};
use iptv_media::{
    AcquireError, HttpTsSessionKey, HttpTsSessionManager, HttpTsSessionSnapshot, HttpTsSourceSpec,
    MpegTsRingConfig, PoolSnapshot, ProviderSpec, SessionFailureKind, SessionStartError,
    SessionState,
};
use iptv_persistence::{
    CatalogRepository, ChannelQuery, Database, JobRecord, JobRepository, MasterKey, NewSource,
    PersistenceError, ProgrammeQuery, SourceKind, SourceRepository, SourceSummary,
    redact_diagnostics, redact_error,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tower_http::{catch_panic::CatchPanicLayer, compression::CompressionLayer};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use crate::auth::{AuthManager, LoginError};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub public_base_url: String,
    pub output_token: String,
    pub admin_bootstrap_token: String,
    pub admin_password_hash: String,
    pub master_key: MasterKey,
    pub tuner_count: u16,
    pub runtime_versions: RuntimeVersions,
}

#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub struct RuntimeVersions {
    pub gateway: String,
    pub ffmpeg: Option<String>,
    pub vlc: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppState {
    database: Option<Database>,
    catalog: Arc<RwLock<CatalogSnapshot>>,
    public_base_url: Arc<str>,
    output_token_hash: [u8; 32],
    auth: AuthManager,
    tuner_count: u16,
    runtime_versions: RuntimeVersions,
    started_at: Instant,
    media: HttpTsSessionManager,
    source_repository: Option<SourceRepository>,
    job_repository: Option<JobRepository>,
    catalog_repository: Option<CatalogRepository>,
    jellyfin: Arc<RwLock<Option<JellyfinConfigRequest>>>,
}

impl AppState {
    pub fn new(database: Option<Database>, config: AppConfig) -> Self {
        let secure_cookies = config.public_base_url.starts_with("https://");
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
            auth: AuthManager::new(
                config.admin_password_hash,
                &config.admin_bootstrap_token,
                secure_cookies,
            ),
            tuner_count: config.tuner_count,
            runtime_versions: config.runtime_versions,
            started_at: Instant::now(),
            media: HttpTsSessionManager::new(reqwest::Client::new()),
            source_repository,
            job_repository,
            catalog_repository,
            jellyfin: Arc::new(RwLock::new(None)),
        }
    }

    pub fn catalog(&self) -> Arc<RwLock<CatalogSnapshot>> {
        Arc::clone(&self.catalog)
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

#[derive(Debug, Deserialize, ToSchema)]
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

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct JellyfinConfigRequest {
    base_url: String,
    tuner_name: String,
    public_base_url: String,
    guide_days: u16,
}

#[derive(Debug, Serialize, ToSchema)]
struct SaveResult {
    ok: bool,
    message: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
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
}

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
    paths(auth_status, login, logout, system_info, settings_schema, list_sources, create_source, list_jobs, cancel_job, list_channels, create_channel, list_programmes, list_events, list_sessions, session_events, catalog_events, save_jellyfin),
    components(schemas(LoginRequest, LogoutRequest, AuthUser, AuthStatus, RuntimeVersions, SystemInfo, SettingDefinition, SourceResponse, CreateSourceRequest, JobResponse, ChannelRecord, ChannelResponse, ChannelPageResponse, CreateChannelRequest, ProgrammeResponse, ProgrammePageResponse, PageQuery, DynamicEventResponse, SessionResponse, JellyfinConfigRequest, SaveResult, ProblemDetails)),
    tags((name = "authentication"), (name = "system"), (name = "settings"), (name = "sources"), (name = "jobs"), (name = "channels"), (name = "guide"), (name = "sessions"), (name = "configuration"))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    let control = Router::new()
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/login", axum::routing::post(login))
        .route("/api/v1/auth/logout", axum::routing::post(logout))
        .route("/api/v1/system", get(system_info))
        .route("/api/v1/settings/schema", get(settings_schema))
        .route("/api/v1/sources", get(list_sources).post(create_source))
        .route("/api/v1/jobs", get(list_jobs))
        .route(
            "/api/v1/jobs/{job_id}/cancel",
            axum::routing::post(cancel_job),
        )
        .route("/api/v1/channels", get(list_channels).post(create_channel))
        .route("/api/v1/programmes", get(list_programmes))
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/sessions", get(list_sessions))
        .route("/api/v1/session-events", get(session_events))
        .route("/api/v1/catalog-events", get(catalog_events))
        .route("/api/v1/jellyfin", axum::routing::put(save_jellyfin))
        .route("/api/v1/openapi.json", get(openapi));

    let output = Router::new()
        .route("/out/{token}/playlist.m3u", get(playlist))
        .route("/out/{token}/xmltv.xml", get(xmltv))
        .route("/out/{token}/stream/{*channel_path}", get(stream_channel))
        .route("/out/{token}/hdhr/discover.json", get(hdhr_discover))
        .route("/out/{token}/hdhr/lineup.json", get(hdhr_lineup))
        .route(
            "/out/{token}/hdhr/lineup_status.json",
            get(hdhr_lineup_status),
        )
        .route("/out/{token}/hdhr/device.xml", get(hdhr_device));

    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .merge(control)
        .merge(output)
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .with_state(state)
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
    let mut response = Json(auth_status_body(authenticated)).into_response();
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
    let session = match state
        .auth
        .login(&headers, request.username, request.password)
        .await
    {
        Ok(session) => session,
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
    let mut response = Json(auth_status_body(true)).into_response();
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

fn auth_status_body(authenticated: bool) -> AuthStatus {
    AuthStatus {
        authenticated,
        user: authenticated.then(|| AuthUser {
            id: "operator-1".to_owned(),
            username: "operator".to_owned(),
            display_name: "Relay operator".to_owned(),
        }),
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
        PersistenceError::InvalidMasterKey
        | PersistenceError::Encryption
        | PersistenceError::Decryption
        | PersistenceError::Database(_)
        | PersistenceError::Migration(_)
        | PersistenceError::JobOwnership { .. } => persistence_unavailable(),
    }
}

#[utoipa::path(get, path = "/api/v1/system", tag = "system", responses((status = 200, body = SystemInfo)))]
#[allow(clippy::cast_precision_loss)]
async fn system_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
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
    Json(SystemInfo {
        channels: usize::try_from(channels).unwrap_or(usize::MAX),
        healthy_streams: usize::try_from(healthy_streams).unwrap_or(usize::MAX),
        active_sessions: provider_connections,
        guide_coverage,
        provider_connections,
        provider_limit,
        uptime_seconds: state.started_at.elapsed().as_secs(),
        versions: state.runtime_versions,
    })
    .into_response()
}

#[utoipa::path(get, path = "/api/v1/settings/schema", tag = "settings", responses((status = 200, body = [SettingDefinition]), (status = 401, body = ProblemDetails)))]
async fn settings_schema(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    Json(setting_catalog()).into_response()
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
    match repository.create(&source, "operator").await {
        Ok(created) => (
            StatusCode::CREATED,
            Json(SourceResponse::from(&created.source)),
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

#[utoipa::path(get, path = "/api/v1/channels", tag = "channels", params(("search" = Option<String>, Query, description = "Case-insensitive name or group fragment"), ("group" = Option<String>, Query, description = "Exact group name"), ("limit" = Option<i64>, Query, description = "Page size, 1-500, default 100"), ("offset" = Option<i64>, Query, description = "Zero-based page offset")), responses((status = 200, body = ChannelPageResponse)))]
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

#[utoipa::path(get, path = "/api/v1/events", tag = "guide", responses((status = 200, body = [DynamicEventResponse])))]
async fn list_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    Json(Vec::<DynamicEventResponse>::new()).into_response()
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
    let stream = async_stream::stream! {
        loop {
            if let Some(catalog) = &catalog_repository
                && let Ok(counts) = catalog.system_counts().await
            {
                let data = serde_json::to_string(&counts).expect("system counts serialize");
                yield Ok::<Event, Infallible>(Event::default().event("overview").data(data));
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
    put,
    path = "/api/v1/jellyfin",
    tag = "configuration",
    request_body = JellyfinConfigRequest,
    responses((status = 200, body = SaveResult), (status = 422, body = ProblemDetails))
)]
async fn save_jellyfin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<JellyfinConfigRequest>,
) -> Response {
    if let Some(response) = require_admin_mutation(&state, &headers) {
        return response;
    }
    let valid_url = |value: &str| {
        url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
    };
    if request.tuner_name.trim().is_empty()
        || request.tuner_name.len() > 160
        || !(1..=30).contains(&request.guide_days)
        || !valid_url(&request.base_url)
        || !valid_url(&request.public_base_url)
    {
        return ProblemDetails::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-jellyfin-configuration",
            "Invalid Jellyfin configuration",
            "provide HTTP(S) URLs, a tuner name, and 1-30 guide days",
        )
        .response();
    }
    let message = format!(
        "Saved tuner “{}” with {} guide days.",
        request.tuner_name.trim(),
        request.guide_days
    );
    *state.jellyfin.write().await = Some(request);
    Json(SaveResult { ok: true, message }).into_response()
}

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

async fn playlist(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
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
    text_response("application/vnd.apple.mpegurl; charset=utf-8", body)
}

async fn xmltv(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
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
    text_response("application/xml; charset=utf-8", body)
}

async fn stream_channel(
    State(state): State<AppState>,
    Path((token, channel_path)): Path<(String, String)>,
) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
    }
    let Some(channel_id) = channel_path
        .strip_suffix(".ts")
        .and_then(|value| Uuid::parse_str(value).ok())
    else {
        return not_found();
    };
    let source = {
        let catalog = state.catalog.read().await;
        if !catalog
            .channels
            .iter()
            .any(|channel| channel.id == channel_id && channel.enabled)
        {
            return not_found();
        }
        catalog.stream_sources.get(&channel_id).cloned()
    };
    let Some(source) = source else {
        return retryable_problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "stream-source-unavailable",
            "Stream source unavailable",
            "the channel has no active provider stream",
        );
    };

    let ring = match default_ring(source.estimated_bitrate_bits_per_second) {
        Ok(ring) => ring,
        Err(detail) => {
            return ProblemDetails::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid-buffer-policy",
                "Invalid buffer policy",
                detail,
            )
            .response();
        }
    };
    let spec = HttpTsSourceSpec::new(
        HttpTsSessionKey::new(
            Arc::clone(&source.provider_pool_id),
            Arc::clone(&source.source_id),
            source.generation,
        ),
        Arc::clone(&source.upstream_url),
        ring,
    );
    match state.media.open(spec).await {
        Ok(viewer) => {
            let mut response = Response::new(Body::from_stream(viewer.into_byte_stream()));
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
            response
        }
        Err(SessionStartError::Provider(AcquireError::AtCapacity { .. })) => retryable_problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider-capacity-exhausted",
            "Provider capacity exhausted",
            "all configured provider connections are in use",
        ),
        Err(SessionStartError::Provider(AcquireError::LeaseIdExhausted)) => ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider-lease-id-exhausted",
            "Provider lease allocation failed",
            "the media-session lease counter was exhausted",
        )
        .response(),
        Err(SessionStartError::UnknownProvider { .. }) => ProblemDetails::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider-not-configured",
            "Provider is not configured",
            "the selected stream references a missing provider pool",
        )
        .response(),
        Err(SessionStartError::StartupTimeout) => retryable_problem(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream-startup-timeout",
            "Upstream startup timed out",
            "the provider did not return response headers before the configured timeout",
        ),
        Err(SessionStartError::Http { message }) => retryable_problem(
            StatusCode::BAD_GATEWAY,
            "upstream-start-failed",
            "Upstream stream failed to start",
            message.to_string(),
        ),
    }
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
    if !valid_output_token(&state, &token) {
        return not_found();
    }
    Json(HdhrDiscover {
        friendly_name: "IPTV Gateway",
        manufacturer: "IPTV Gateway",
        model_number: "HDHR-IPTV",
        firmware_name: "iptv-gateway",
        firmware_version: env!("CARGO_PKG_VERSION").to_owned(),
        device_id: "FFFFFFFF",
        device_auth: output_token_fingerprint(&token),
        base_url: format!("{}/out/{}/hdhr", state.public_base_url, token),
        lineup_url: format!("{}/out/{}/hdhr/lineup.json", state.public_base_url, token),
        tuner_count: state.tuner_count,
    })
    .into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct HdhrChannel {
    guide_number: String,
    guide_name: String,
    url: String,
}

async fn hdhr_lineup(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
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
    Json(channels).into_response()
}

async fn hdhr_lineup_status(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
    }
    Json(serde_json::json!({
        "ScanInProgress": 0,
        "ScanPossible": 1,
        "Source": "Cable",
        "SourceList": ["Cable"]
    }))
    .into_response()
}

async fn hdhr_device(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !valid_output_token(&state, &token) {
        return not_found();
    }
    let xml = format!(
        "<?xml version=\"1.0\"?><root><device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType><friendlyName>IPTV Gateway</friendlyName><manufacturer>IPTV Gateway</manufacturer><modelName>HDHR-IPTV</modelName><UDN>uuid:{}</UDN></device></root>",
        Uuid::nil()
    );
    text_response("application/xml; charset=utf-8", xml)
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    state.auth.authorize(headers, false).is_none().then(|| {
        ProblemDetails::new(
            StatusCode::UNAUTHORIZED,
            "authentication-required",
            "Authentication required",
            "sign in as the local administrator or provide the bootstrap bearer token",
        )
        .response()
    })
}

fn require_admin_mutation(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if state.auth.authorize(headers, true).is_some() {
        return None;
    }
    Some(if state.auth.authorize(headers, false).is_some() {
        ProblemDetails::new(
            StatusCode::FORBIDDEN,
            "csrf-validation-failed",
            "CSRF validation failed",
            "provide the CSRF token associated with the administrator session",
        )
        .response()
    } else {
        ProblemDetails::new(
            StatusCode::UNAUTHORIZED,
            "authentication-required",
            "Authentication required",
            "sign in as the local administrator or provide the bootstrap bearer token",
        )
        .response()
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

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use iptv_media::RingSnapshot;
    use tower::ServiceExt;

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
            },
        )
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

    #[tokio::test]
    async fn health_is_public() {
        let response = router(state())
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
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

        let invalid = app
            .clone()
            .oneshot(
                Request::put("/api/v1/jellyfin")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"baseUrl":"invalid","tunerName":"","publicBaseUrl":"http://gateway.test","guideDays":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let saved = app
            .oneshot(
                Request::put("/api/v1/jellyfin")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"baseUrl":"http://jellyfin:8096","tunerName":"Relay","publicBaseUrl":"http://gateway.test","guideDays":7}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert!(response_text(saved).await.contains("7 guide days"));
        assert_eq!(
            app_state.jellyfin.read().await.as_ref().unwrap().guide_days,
            7
        );
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
    fn default_ring_is_eight_seconds_with_a_hard_byte_ceiling() {
        let ordinary = default_ring(8_000_000).unwrap();
        assert!(ordinary.capacity_bytes() < 64 * 1024 * 1024);
        assert!(ordinary.pre_roll_packets() < ordinary.capacity_packets());

        let enormous = default_ring(u64::MAX).unwrap();
        assert!(enormous.capacity_bytes() <= 64 * 1024 * 1024);
        assert!(enormous.pre_roll_packets() <= enormous.capacity_packets());
    }
}
