//! Loopback credential broker for fixed media processes.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::get,
};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{InputSource, InputSourceError};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_HLS_TOKEN_TTL: Duration = Duration::from_secs(30);
const DEFAULT_HLS_TOKEN_REQUESTS: usize = 4;
const DEFAULT_HLS_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_HLS_MAX_URI_BYTES: usize = 8 * 1024;
const DEFAULT_HLS_MAX_TOKENS: usize = 4_096;

/// One provider endpoint for a process session.
#[derive(Clone, Eq, PartialEq)]
pub struct CredentialBrokerEndpoint {
    url: Arc<str>,
    headers: HeaderMap,
}

impl fmt::Debug for CredentialBrokerEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialBrokerEndpoint")
            .field("url", &"<redacted>")
            .field(
                "header_names",
                &self
                    .headers
                    .keys()
                    .map(axum::http::HeaderName::as_str)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CredentialBrokerEndpoint {
    pub fn new(url: impl Into<Arc<str>>) -> Self {
        Self {
            url: url.into(),
            headers: HeaderMap::new(),
        }
    }

    /// Header values can contain credentials. Debug output does not include the values.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }
}

/// Bounds for one HLS credential-broker session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HlsBrokerConfig {
    token_ttl: Duration,
    max_requests_per_token: usize,
    max_response_bytes: usize,
    max_uri_bytes: usize,
    max_tokens: usize,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HlsBrokerConfigError {
    #[error("the HLS token lifetime must be greater than zero")]
    ZeroTokenLifetime,
    #[error("the HLS token request limit must be greater than zero")]
    ZeroTokenRequests,
    #[error("the HLS response-size limit must be greater than zero")]
    ZeroResponseSize,
    #[error("the HLS URI-size limit must be greater than zero")]
    ZeroUriSize,
    #[error("the HLS token limit must be greater than zero")]
    ZeroTokens,
}

impl Default for HlsBrokerConfig {
    fn default() -> Self {
        Self {
            token_ttl: DEFAULT_HLS_TOKEN_TTL,
            max_requests_per_token: DEFAULT_HLS_TOKEN_REQUESTS,
            max_response_bytes: DEFAULT_HLS_MAX_RESPONSE_BYTES,
            max_uri_bytes: DEFAULT_HLS_MAX_URI_BYTES,
            max_tokens: DEFAULT_HLS_MAX_TOKENS,
        }
    }
}

impl HlsBrokerConfig {
    /// Create bounded HLS broker settings.
    ///
    /// # Errors
    ///
    /// Returns an error when a required limit is zero.
    pub const fn new(
        token_ttl: Duration,
        max_requests_per_token: usize,
        max_response_bytes: usize,
        max_uri_bytes: usize,
        max_tokens: usize,
    ) -> Result<Self, HlsBrokerConfigError> {
        if token_ttl.is_zero() {
            return Err(HlsBrokerConfigError::ZeroTokenLifetime);
        }
        if max_requests_per_token == 0 {
            return Err(HlsBrokerConfigError::ZeroTokenRequests);
        }
        if max_response_bytes == 0 {
            return Err(HlsBrokerConfigError::ZeroResponseSize);
        }
        if max_uri_bytes == 0 {
            return Err(HlsBrokerConfigError::ZeroUriSize);
        }
        if max_tokens == 0 {
            return Err(HlsBrokerConfigError::ZeroTokens);
        }
        Ok(Self {
            token_ttl,
            max_requests_per_token,
            max_response_bytes,
            max_uri_bytes,
            max_tokens,
        })
    }

    fn validate(self) -> Result<(), HlsBrokerConfigError> {
        let _ = Self::new(
            self.token_ttl,
            self.max_requests_per_token,
            self.max_response_bytes,
            self.max_uri_bytes,
            self.max_tokens,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CredentialBrokerError {
    #[error("the loopback credential broker could not bind ({kind:?})")]
    Bind { kind: std::io::ErrorKind },
    #[error("the loopback credential broker produced an invalid process URL")]
    InputSource(#[from] InputSourceError),
    #[error("the HLS credential broker endpoint is invalid")]
    InvalidHlsEndpoint,
    #[error(transparent)]
    HlsConfig(#[from] HlsBrokerConfigError),
    #[error("the loopback credential broker task stopped unexpectedly")]
    TaskStopped,
}

/// A one-request loopback broker for one process session.
pub struct CredentialBroker {
    input_source: InputSource,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl fmt::Debug for CredentialBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialBroker")
            .field("input_source", &self.input_source)
            .field("active", &self.shutdown.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for CredentialBroker {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl CredentialBroker {
    /// Start a broker on an operating-system assigned IPv4 loopback port.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the listener or process URL cannot start.
    pub async fn start(
        client: Client,
        endpoint: CredentialBrokerEndpoint,
    ) -> Result<Self, CredentialBrokerError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| CredentialBrokerError::Bind { kind: error.kind() })?;
        let address = listener
            .local_addr()
            .map_err(|error| CredentialBrokerError::Bind { kind: error.kind() })?;
        let token = new_session_token();
        let token_hash = digest(token.as_bytes());
        let state = BrokerState {
            client,
            endpoint,
            token_hash,
            consumed: Arc::new(AtomicBool::new(false)),
        };
        let app = Router::new()
            .route("/session/{token}", get(proxy_session))
            .with_state(state);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        let broker_url = format!("http://{address}/session/{token}");
        let input_source = InputSource::loopback_http(&broker_url)?;
        Ok(Self {
            input_source,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    /// Start a bounded HLS manifest and resource broker.
    ///
    /// The input URL has no provider credentials. Each rewritten manifest URI
    /// has a short-lived, request-limited loopback token.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the endpoint, listener, or input URL is invalid.
    pub async fn start_hls(
        client: Client,
        endpoint: CredentialBrokerEndpoint,
        config: HlsBrokerConfig,
    ) -> Result<Self, CredentialBrokerError> {
        config.validate()?;
        let manifest_url = validated_hls_url(endpoint.url.as_ref(), config.max_uri_bytes)
            .map_err(|()| CredentialBrokerError::InvalidHlsEndpoint)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| CredentialBrokerError::Bind { kind: error.kind() })?;
        let address = listener
            .local_addr()
            .map_err(|error| CredentialBrokerError::Bind { kind: error.kind() })?;
        let base_url: Arc<str> = format!("http://{address}/hls").into();
        let session_token = new_session_token();
        let state = HlsBrokerState {
            client,
            endpoint,
            config,
            base_url: Arc::clone(&base_url),
            tokens: Arc::new(Mutex::new(HashMap::new())),
        };
        state
            .insert_token(
                session_token.clone(),
                manifest_url,
                HlsResourceKind::Manifest,
            )
            .await
            .map_err(|()| CredentialBrokerError::InvalidHlsEndpoint)?;
        let app = Router::new()
            .route("/hls/{token}/{resource}", get(proxy_hls_resource))
            .with_state(state);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        let input_source =
            InputSource::loopback_http(&format!("{base_url}/{session_token}/manifest.m3u8"))?;
        Ok(Self {
            input_source,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    pub fn input_source(&self) -> &InputSource {
        &self.input_source
    }

    pub fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Stop the listener and wait for the broker task.
    ///
    /// If the task does not stop in two seconds, abort the task.
    ///
    /// # Errors
    ///
    /// Returns an error if the broker task panics or ends unexpectedly.
    pub async fn shutdown(mut self) -> Result<(), CredentialBrokerError> {
        self.request_shutdown();
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(CredentialBrokerError::TaskStopped),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Ok(())
            }
        }
    }
}

#[derive(Clone)]
struct BrokerState {
    client: Client,
    endpoint: CredentialBrokerEndpoint,
    token_hash: [u8; 32],
    consumed: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HlsResourceKind {
    Manifest,
    Binary,
}

#[derive(Clone)]
struct HlsResourceToken {
    url: reqwest::Url,
    kind: HlsResourceKind,
    expires_at: Instant,
    remaining_requests: usize,
}

#[derive(Clone)]
struct HlsBrokerState {
    client: Client,
    endpoint: CredentialBrokerEndpoint,
    config: HlsBrokerConfig,
    base_url: Arc<str>,
    tokens: Arc<Mutex<HashMap<[u8; 32], HlsResourceToken>>>,
}

impl fmt::Debug for HlsBrokerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsBrokerState")
            .field("endpoint", &self.endpoint)
            .field("config", &self.config)
            .field("base_url", &"http://<loopback>/hls")
            .field("tokens", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HlsBrokerState {
    async fn insert_token(
        &self,
        token: String,
        url: reqwest::Url,
        kind: HlsResourceKind,
    ) -> Result<(), ()> {
        let mut tokens = self.tokens.lock().await;
        tokens.retain(|_, resource| {
            resource.expires_at > Instant::now() && resource.remaining_requests > 0
        });
        if tokens.len() >= self.config.max_tokens {
            return Err(());
        }
        tokens.insert(
            digest(token.as_bytes()),
            HlsResourceToken {
                url,
                kind,
                expires_at: Instant::now() + self.config.token_ttl,
                remaining_requests: self.config.max_requests_per_token,
            },
        );
        Ok(())
    }

    async fn take_token(&self, token: &str) -> Option<HlsResourceToken> {
        let key = digest(token.as_bytes());
        let mut tokens = self.tokens.lock().await;
        let resource = tokens.get_mut(&key)?;
        if resource.expires_at <= Instant::now() || resource.remaining_requests == 0 {
            tokens.remove(&key);
            return None;
        }
        resource.remaining_requests -= 1;
        Some(resource.clone())
    }

    async fn rewrite_url(
        &self,
        base: &reqwest::Url,
        raw_uri: &str,
        kind: HlsResourceKind,
    ) -> Result<String, ()> {
        let url = resolve_hls_uri(base, raw_uri, self.config.max_uri_bytes)?;
        let kind = if matches!(kind, HlsResourceKind::Manifest) || is_manifest_url(&url) {
            HlsResourceKind::Manifest
        } else {
            HlsResourceKind::Binary
        };
        let token = new_session_token();
        self.insert_token(token.clone(), url, kind).await?;
        let resource_name = match kind {
            HlsResourceKind::Manifest => "manifest.m3u8",
            HlsResourceKind::Binary => hls_resource_name(raw_uri),
        };
        Ok(format!("{}/{token}/{resource_name}", self.base_url))
    }
}

fn hls_resource_name(raw_uri: &str) -> &str {
    let path = raw_uri.split(['?', '#']).next().unwrap_or_default();
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 16
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    match extension {
        Some(extension) => match extension {
            "ts" => "resource.ts",
            "m4s" => "resource.m4s",
            "mp4" => "resource.mp4",
            "key" => "resource.key",
            _ => "resource.bin",
        },
        None => "resource.bin",
    }
}

impl fmt::Debug for BrokerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokerState")
            .field("endpoint", &self.endpoint)
            .field("token_hash", &"<redacted>")
            .field("consumed", &self.consumed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

async fn proxy_hls_resource(
    State(state): State<HlsBrokerState>,
    Path((token, _resource)): Path<(String, String)>,
) -> Response {
    let Some(resource) = state.take_token(&token).await else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    let Ok((headers, bytes)) = fetch_hls_resource(&state, resource.url.clone()).await else {
        return empty_response(StatusCode::BAD_GATEWAY);
    };

    let body = match resource.kind {
        HlsResourceKind::Manifest => {
            match rewrite_hls_manifest(&state, &resource.url, &bytes).await {
                Ok(manifest) => Bytes::from(manifest),
                Err(()) => return empty_response(StatusCode::BAD_GATEWAY),
            }
        }
        HlsResourceKind::Binary => bytes,
    };
    let mut response = Response::new(Body::from(body));
    let response_headers = response.headers_mut();
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        response_headers.insert(header::CONTENT_TYPE, content_type.clone());
    } else if matches!(resource.kind, HlsResourceKind::Manifest) {
        response_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.apple.mpegurl"),
        );
    }
    response
}

async fn fetch_hls_resource(
    state: &HlsBrokerState,
    url: reqwest::Url,
) -> Result<(HeaderMap, Bytes), ()> {
    let upstream = state
        .client
        .get(url)
        .headers(state.endpoint.headers.clone())
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?;
    if upstream
        .content_length()
        .is_some_and(|length| length > u64::try_from(state.config.max_response_bytes).unwrap())
    {
        return Err(());
    }
    let headers = upstream.headers().clone();
    let mut stream = upstream.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        if bytes.len().saturating_add(chunk.len()) > state.config.max_response_bytes {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((headers, Bytes::from(bytes)))
}

async fn rewrite_hls_manifest(
    state: &HlsBrokerState,
    base: &reqwest::Url,
    body: &[u8],
) -> Result<String, ()> {
    if body.len() > state.config.max_response_bytes {
        return Err(());
    }
    let manifest = std::str::from_utf8(body).map_err(|_| ())?;
    let manifest = manifest.strip_prefix('\u{feff}').unwrap_or(manifest);
    let mut lines = manifest.lines();
    if lines.next() != Some("#EXTM3U") {
        return Err(());
    }
    let mut rewritten = String::from("#EXTM3U\n");
    for line in lines {
        if line.len() > state.config.max_uri_bytes || line.contains('\0') {
            return Err(());
        }
        if line.starts_with('#') {
            rewritten.push_str(&rewrite_hls_attributes(state, base, line).await?);
        } else if line.trim().is_empty() {
            rewritten.push_str(line);
        } else {
            if line.trim() != line {
                return Err(());
            }
            rewritten.push_str(
                &state
                    .rewrite_url(base, line, HlsResourceKind::Binary)
                    .await?,
            );
        }
        rewritten.push('\n');
    }
    Ok(rewritten)
}

async fn rewrite_hls_attributes(
    state: &HlsBrokerState,
    base: &reqwest::Url,
    line: &str,
) -> Result<String, ()> {
    let mut output = String::new();
    let mut remaining = line;
    loop {
        let Some(offset) = remaining.find("URI=") else {
            output.push_str(remaining);
            return Ok(output);
        };
        let attribute_start = offset == 0
            || remaining.as_bytes()[offset.saturating_sub(1)] == b','
            || remaining.as_bytes()[offset.saturating_sub(1)] == b':';
        if !attribute_start {
            let split = offset.saturating_add(4);
            output.push_str(&remaining[..split]);
            remaining = &remaining[split..];
            continue;
        }
        output.push_str(&remaining[..offset + 4]);
        let after_key = &remaining[offset + 4..];
        let Some(uri_tail) = after_key.strip_prefix('"') else {
            return Err(());
        };
        let Some(end) = uri_tail.find('"') else {
            return Err(());
        };
        let uri = &uri_tail[..end];
        output.push('"');
        output.push_str(
            &state
                .rewrite_url(base, uri, HlsResourceKind::Binary)
                .await?,
        );
        output.push('"');
        remaining = &uri_tail[end + 1..];
    }
}

fn validated_hls_url(value: &str, max_uri_bytes: usize) -> Result<reqwest::Url, ()> {
    if value.len() > max_uri_bytes || value.contains('\0') {
        return Err(());
    }
    let url = reqwest::Url::parse(value).map_err(|_| ())?;
    validate_hls_url(&url)?;
    Ok(url)
}

fn resolve_hls_uri(
    base: &reqwest::Url,
    value: &str,
    max_uri_bytes: usize,
) -> Result<reqwest::Url, ()> {
    if value.is_empty()
        || value.len() > max_uri_bytes
        || value.contains('\0')
        || value.starts_with("//")
        || contains_traversal(value)
    {
        return Err(());
    }
    let url = base.join(value).map_err(|_| ())?;
    validate_hls_url(&url)?;
    Ok(url)
}

fn validate_hls_url(url: &reqwest::Url) -> Result<(), ()> {
    if !matches!(url.scheme(), "http" | "https") || url.fragment().is_some() {
        return Err(());
    }
    Ok(())
}

fn contains_traversal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return true;
    }
    value
        .split(['/', '\\'])
        .map(str::to_ascii_lowercase)
        .any(|segment| matches!(segment.as_str(), ".." | "%2e%2e" | ".%2e" | "%2e."))
}

fn is_manifest_url(url: &reqwest::Url) -> bool {
    std::path::Path::new(url.path())
        .extension()
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("m3u8") || extension.eq_ignore_ascii_case("m3u")
        })
}

async fn proxy_session(State(state): State<BrokerState>, Path(token): Path<String>) -> Response {
    if !constant_time_eq(&digest(token.as_bytes()), &state.token_hash) {
        return empty_response(StatusCode::NOT_FOUND);
    }
    if state
        .consumed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return empty_response(StatusCode::NOT_FOUND);
    }

    let Ok(upstream) = state
        .client
        .get(state.endpoint.url.as_ref())
        .headers(state.endpoint.headers.clone())
        .send()
        .await
    else {
        return empty_response(StatusCode::BAD_GATEWAY);
    };
    if !upstream.status().is_success() {
        return empty_response(StatusCode::BAD_GATEWAY);
    }

    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let stream = upstream
        .bytes_stream()
        .map(|chunk| chunk.map_err(|_| std::io::Error::other("upstream media read failed")));
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Some(content_type) = content_type {
        headers.insert(header::CONTENT_TYPE, content_type);
    }
    response
}

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

fn new_session_token() -> String {
    format!(
        "{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicUsize};

    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode, header},
        routing::get,
    };

    use super::*;

    #[test]
    fn tokens_have_more_than_256_bits_of_random_input() {
        let first = new_session_token();
        let second = new_session_token();
        assert_eq!(first.len(), 96);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert!(constant_time_eq(
            &digest(first.as_bytes()),
            &digest(first.as_bytes())
        ));
        assert!(!constant_time_eq(
            &digest(first.as_bytes()),
            &digest(second.as_bytes())
        ));
    }

    #[test]
    fn hls_configuration_rejects_zero_limits() {
        assert_eq!(
            HlsBrokerConfig::new(Duration::ZERO, 1, 1, 1, 1),
            Err(HlsBrokerConfigError::ZeroTokenLifetime)
        );
        assert_eq!(
            HlsBrokerConfig::new(Duration::from_secs(1), 0, 1, 1, 1),
            Err(HlsBrokerConfigError::ZeroTokenRequests)
        );
        assert_eq!(
            HlsBrokerConfig::new(Duration::from_secs(1), 1, 0, 1, 1),
            Err(HlsBrokerConfigError::ZeroResponseSize)
        );
        assert_eq!(
            HlsBrokerConfig::new(Duration::from_secs(1), 1, 1, 0, 1),
            Err(HlsBrokerConfigError::ZeroUriSize)
        );
        assert_eq!(
            HlsBrokerConfig::new(Duration::from_secs(1), 1, 1, 1, 0),
            Err(HlsBrokerConfigError::ZeroTokens)
        );
    }

    #[tokio::test]
    async fn broker_proxies_one_request_and_redacts_all_credentials() {
        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
        }

        async fn upstream(State(state): State<UpstreamState>, headers: HeaderMap) -> String {
            state.requests.fetch_add(1, Ordering::SeqCst);
            let authorization = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let cookie = headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            format!("{authorization}|{cookie}")
        }

        let requests = Arc::new(AtomicUsize::new(0));
        let upstream_listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_requests = Arc::clone(&requests);
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new()
                    .route("/provider/live", get(upstream))
                    .with_state(UpstreamState {
                        requests: upstream_requests,
                    }),
            )
            .await
            .unwrap();
        });

        let provider_url = format!(
            "http://{upstream_address}/provider/live?username=provider-user&password=provider-password"
        );
        let mut endpoint = CredentialBrokerEndpoint::new(provider_url.clone());
        endpoint.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-secret"),
        );
        endpoint.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("session=provider-cookie"),
        );
        let broker = CredentialBroker::start(Client::new(), endpoint.clone())
            .await
            .unwrap();
        let broker_debug = format!("{broker:?} {endpoint:?}");
        for secret in [
            "provider-user",
            "provider-password",
            "provider-secret",
            "provider-cookie",
        ] {
            assert!(!broker_debug.contains(secret));
        }

        let process_url = broker.input_source.broker_url().as_str().to_owned();
        assert!(process_url.starts_with("http://127.0.0.1:"));
        assert!(!process_url.contains("provider-"));
        let response = Client::new().get(&process_url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = response.text().await.unwrap();
        assert_eq!(body, "Bearer provider-secret|session=provider-cookie");

        let replay = Client::new().get(&process_url).send().await.unwrap();
        assert_eq!(replay.status(), StatusCode::NOT_FOUND);
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        broker.shutdown().await.unwrap();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn upstream_failures_return_only_a_redacted_gateway_error() {
        let endpoint = CredentialBrokerEndpoint::new(
            "http://127.0.0.1:1/live?username=secret&password=secret",
        );
        let broker = CredentialBroker::start(Client::new(), endpoint)
            .await
            .unwrap();
        let response = Client::new()
            .get(broker.input_source.broker_url().as_str())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(response.text().await.unwrap().is_empty());
        broker.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn hls_broker_rewrites_nested_playlists_segments_keys_and_maps() {
        use std::{collections::HashMap, sync::Mutex};

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            response::Response,
            routing::get,
        };

        #[derive(Clone)]
        struct UpstreamState {
            address: Arc<Mutex<Option<std::net::SocketAddr>>>,
            requests: Arc<Mutex<HashMap<String, usize>>>,
        }

        async fn upstream(
            State(state): State<UpstreamState>,
            Path(path): Path<String>,
            headers: HeaderMap,
        ) -> Response {
            assert_eq!(
                headers.get(header::AUTHORIZATION),
                Some(&HeaderValue::from_static("Bearer provider-secret"))
            );
            *state
                .requests
                .lock()
                .unwrap()
                .entry(path.clone())
                .or_default() += 1;
            let address = state.address.lock().unwrap().unwrap();
            let body = match path.as_str() {
                "master.m3u8" => format!(
                    "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nnested/child.m3u8\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key.bin\"\n#EXT-X-MAP:URI=\"/maps/init.mp4\"\nsegments/one.ts\nhttp://{address}/absolute/two.ts?provider-token=secret\n"
                ),
                "nested/child.m3u8" => "#EXTM3U\n#EXTINF:2,Child\nchild/two.ts\n".to_owned(),
                "keys/key.bin" => "key-data".to_owned(),
                "maps/init.mp4" => "map-data".to_owned(),
                "segments/one.ts" => "segment-one".to_owned(),
                "absolute/two.ts" => "segment-two".to_owned(),
                _ => "missing".to_owned(),
            };
            let mut response = Response::new(Body::from(body));
            if std::path::Path::new(&path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m3u8"))
            {
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.apple.mpegurl"),
                );
            }
            response
        }

        let state = UpstreamState {
            address: Arc::new(Mutex::new(None)),
            requests: Arc::new(Mutex::new(HashMap::new())),
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        *state.address.lock().unwrap() = Some(address);
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/{*path}", get(upstream))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let mut endpoint = CredentialBrokerEndpoint::new(format!(
            "http://{address}/master.m3u8?username=provider-user&password=provider-password"
        ));
        endpoint.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-secret"),
        );
        let broker = CredentialBroker::start_hls(
            Client::new(),
            endpoint.clone(),
            HlsBrokerConfig::default(),
        )
        .await
        .unwrap();
        let debug = format!("{broker:?} {endpoint:?}");
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(!debug.contains(secret));
        }

        let master_url = broker.input_source().broker_url().as_str().to_owned();
        let master = Client::new()
            .get(&master_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(master.starts_with("#EXTM3U\n"));
        for secret in [
            "provider-user",
            "provider-password",
            "provider-secret",
            "provider-token",
        ] {
            assert!(!master.contains(secret));
        }
        let urls = hls_urls(&master);
        assert_eq!(urls.len(), 5);
        assert!(urls.iter().all(|url| url.starts_with("http://127.0.0.1:")));

        let nested = Client::new()
            .get(&urls[0])
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(nested.starts_with("#EXTM3U\n"));
        let nested_urls = hls_urls(&nested);
        assert_eq!(nested_urls.len(), 1);
        assert_eq!(
            Client::new()
                .get(&nested_urls[0])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        for (url, expected) in
            urls.iter()
                .skip(1)
                .zip(["key-data", "map-data", "segment-one", "segment-two"])
        {
            let response = Client::new().get(url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.text().await.unwrap(), expected);
        }
        {
            let requests = state.requests.lock().unwrap();
            for path in [
                "master.m3u8",
                "nested/child.m3u8",
                "nested/child/two.ts",
                "keys/key.bin",
                "maps/init.mp4",
                "segments/one.ts",
                "absolute/two.ts",
            ] {
                assert_eq!(requests.get(path), Some(&1));
            }
        }

        broker.shutdown().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn hls_broker_rejects_unknown_expired_limited_and_invalid_resources() {
        use axum::{Router, body::Body, extract::Path, response::Response, routing::get};

        async fn upstream(Path(path): Path<String>) -> Response {
            let body = match path.as_str() {
                "valid.m3u8" => "#EXTM3U\n#EXTINF:2,Valid\nsegment.ts\n",
                "malformed.m3u8" => "not-a-manifest\n",
                "traversal.m3u8" => "#EXTM3U\n../segment.ts\n",
                "scheme.m3u8" => "#EXTM3U\nfile:///secret.ts\n",
                "large.m3u8" => "#EXTM3U\n012345678901234567890123456789\n",
                _ => "segment",
            };
            Response::new(Body::from(body))
        }

        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/{*path}", get(upstream)))
                .await
                .unwrap();
        });
        let client = Client::new();

        let config = HlsBrokerConfig::new(Duration::from_millis(20), 1, 24, 128, 8).unwrap();
        let broker = CredentialBroker::start_hls(
            client.clone(),
            CredentialBrokerEndpoint::new(format!("http://{address}/valid.m3u8")),
            config,
        )
        .await
        .unwrap();
        let input_url = broker.input_source().broker_url().as_str().to_owned();
        let unknown = input_url.replacen("/hls/", "/hls/unknown-", 1);
        assert_eq!(
            client.get(unknown).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            client.get(&input_url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        broker.shutdown().await.unwrap();

        for path in [
            "malformed.m3u8",
            "traversal.m3u8",
            "scheme.m3u8",
            "large.m3u8",
        ] {
            let broker = CredentialBroker::start_hls(
                client.clone(),
                CredentialBrokerEndpoint::new(format!("http://{address}/{path}")),
                HlsBrokerConfig::new(Duration::from_secs(1), 1, 24, 128, 8).unwrap(),
            )
            .await
            .unwrap();
            let response = client
                .get(broker.input_source().broker_url().as_str())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{path}");
            broker.shutdown().await.unwrap();
        }

        let broker = CredentialBroker::start_hls(
            client.clone(),
            CredentialBrokerEndpoint::new(format!("http://{address}/valid.m3u8")),
            HlsBrokerConfig::new(Duration::from_secs(1), 1, 1_024, 128, 8).unwrap(),
        )
        .await
        .unwrap();
        let input_url = broker.input_source().broker_url().as_str().to_owned();
        assert_eq!(
            client.get(&input_url).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            client.get(&input_url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        broker.shutdown().await.unwrap();
        server.abort();
    }

    fn hls_urls(manifest: &str) -> Vec<String> {
        manifest
            .lines()
            .flat_map(|line| {
                let mut urls = Vec::new();
                if line.starts_with("http://") {
                    urls.push(line.to_owned());
                }
                let mut remaining = line;
                while let Some(start) = remaining.find("URI=\"") {
                    let after_start = &remaining[start + 5..];
                    let end = after_start.find('"').unwrap();
                    urls.push(after_start[..end].to_owned());
                    remaining = &after_start[end + 1..];
                }
                urls
            })
            .collect()
    }
}
