//! Native HTTP MPEG-TS session sharing.

use std::{
    fmt,
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
    time::Duration,
};

use bytes::{Buf, Bytes, BytesMut};
use dashmap::{DashMap, mapref::entry::Entry};
use futures_util::{Stream, StreamExt};
use reqwest::{Client, Response, header::HeaderMap};
use thiserror::Error;
use tokio::{
    task::AbortHandle,
    time::{Instant, MissedTickBehavior},
};
use tracing::{debug, info, warn};

use crate::{
    AcquireError, BrokeredInputFormat, HttpTsSessionSnapshot, MPEG_TS_PACKET_SIZE, MpegTsRing,
    MpegTsRingConfig, PoolSnapshot, ProcessAdapterKind, ProviderSlotBroker, RecoveryPolicy,
    RingCloseReason, RingRead, RingReadError, RingWriteError, SessionFailureKind, SessionState,
    SharedSessionRegistry, SlotLease,
    psi::PatPmtTracker,
    recovery::{SessionDiagnostics, ViewerRegistration},
};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_VIEWER_BATCH_PACKETS: NonZeroUsize = NonZeroUsize::new(32).unwrap();
const MAX_PRIMING_PACKETS: usize = 16_384;

/// The input-adapter policy for one HTTP MPEG-TS source.
///
/// `Auto` selects native HTTP MPEG-TS input. It does not select an HLS adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputAdapterPolicy {
    #[default]
    Auto,
    NativeTs,
    Ffmpeg,
    Vlc,
}

/// The active adapter for a shared media session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionInputAdapter {
    NativeTs,
    Ffmpeg,
    Vlc,
}

/// Redacted adapter diagnostics for one shared media session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAdapterDiagnostics {
    pub key: HttpTsSessionKey,
    pub adapter: SessionInputAdapter,
    pub process_id: Option<u32>,
}

impl InputAdapterPolicy {
    #[must_use]
    pub const fn selected_adapter(self) -> SessionInputAdapter {
        match self {
            Self::Auto | Self::NativeTs => SessionInputAdapter::NativeTs,
            Self::Ffmpeg => SessionInputAdapter::Ffmpeg,
            Self::Vlc => SessionInputAdapter::Vlc,
        }
    }
}

/// A provider account or explicit connection-sharing pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSpec {
    pub pool_id: Arc<str>,
    pub max_connections: usize,
}

impl ProviderSpec {
    pub fn new(pool_id: impl Into<Arc<str>>, max_connections: usize) -> Self {
        Self {
            pool_id: pool_id.into(),
            max_connections,
        }
    }
}

/// Stable identity for one source configuration generation.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct HttpTsSessionKey {
    pub provider_pool_id: Arc<str>,
    pub source_id: Arc<str>,
    pub generation: u64,
}

impl HttpTsSessionKey {
    pub fn new(
        provider_pool_id: impl Into<Arc<str>>,
        source_id: impl Into<Arc<str>>,
        generation: u64,
    ) -> Self {
        Self {
            provider_pool_id: provider_pool_id.into(),
            source_id: source_id.into(),
            generation,
        }
    }

    fn allocation_key(&self) -> Arc<str> {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.provider_pool_id, self.source_id, self.generation
        )
        .into()
    }
}

/// One credential-bearing HTTP endpoint. Debug output is always redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct HttpTsEndpoint {
    provider_pool_id: Option<Arc<str>>,
    url: Arc<str>,
    headers: HeaderMap,
}

impl fmt::Debug for HttpTsEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsEndpoint")
            .field("provider_pool_id", &self.provider_pool_id)
            .field("url", &"<redacted>")
            .field(
                "header_names",
                &self
                    .headers
                    .keys()
                    .map(reqwest::header::HeaderName::as_str)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl HttpTsEndpoint {
    pub fn new(url: impl Into<Arc<str>>) -> Self {
        Self {
            provider_pool_id: None,
            url: url.into(),
            headers: HeaderMap::new(),
        }
    }

    /// Creates an endpoint that consumes capacity from a specific pool.
    pub fn for_provider(provider_pool_id: impl Into<Arc<str>>, url: impl Into<Arc<str>>) -> Self {
        Self {
            provider_pool_id: Some(provider_pool_id.into()),
            url: url.into(),
            headers: HeaderMap::new(),
        }
    }

    /// Headers may contain credentials; they are never included in debug or
    /// session diagnostic output.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    fn effective_pool_id<'a>(&'a self, default: &'a Arc<str>) -> &'a Arc<str> {
        self.provider_pool_id.as_ref().unwrap_or(default)
    }
}

/// Request, buffering, and bounded recovery policy for an HTTP MPEG-TS source.
///
/// `generation` must change when the effective URL or headers change; active
/// viewers of an older generation are then isolated from the new session.
#[derive(Clone)]
pub struct HttpTsSourceSpec {
    key: HttpTsSessionKey,
    channel_name: Option<Arc<str>>,
    endpoints: Vec<HttpTsEndpoint>,
    adapter_policy: InputAdapterPolicy,
    process_input_format: BrokeredInputFormat,
    ring: MpegTsRingConfig,
    startup_timeout: Duration,
    read_timeout: Duration,
    viewer_batch_packets: NonZeroUsize,
    recovery: RecoveryPolicy,
}

impl fmt::Debug for HttpTsSourceSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsSourceSpec")
            .field("key", &self.key)
            .field("channel_name", &self.channel_name)
            .field("primary", &self.endpoints[0])
            .field("alternate_count", &self.endpoints.len().saturating_sub(1))
            .field("adapter_policy", &self.adapter_policy)
            .field("process_input_format", &self.process_input_format)
            .field("ring", &self.ring)
            .field("startup_timeout", &self.startup_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("viewer_batch_packets", &self.viewer_batch_packets)
            .field("recovery", &self.recovery)
            .finish()
    }
}

impl HttpTsSourceSpec {
    pub fn new(key: HttpTsSessionKey, url: impl Into<Arc<str>>, ring: MpegTsRingConfig) -> Self {
        Self {
            key,
            channel_name: None,
            endpoints: vec![HttpTsEndpoint::new(url)],
            adapter_policy: InputAdapterPolicy::Auto,
            process_input_format: BrokeredInputFormat::DirectMpegTs,
            ring,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            viewer_batch_packets: DEFAULT_VIEWER_BATCH_PACKETS,
            recovery: RecoveryPolicy::default(),
        }
    }

    pub fn key(&self) -> &HttpTsSessionKey {
        &self.key
    }

    /// Adds a redacted display label for diagnostics and operator views.
    pub fn set_channel_name(&mut self, name: impl Into<Arc<str>>) {
        self.channel_name = Some(name.into());
    }

    pub fn adapter_policy(&self) -> InputAdapterPolicy {
        self.adapter_policy
    }

    /// Select the adapter for this source generation.
    ///
    /// Change the generation before you change this value.
    pub fn set_adapter_policy(&mut self, policy: InputAdapterPolicy) {
        self.adapter_policy = policy;
    }

    /// Select the explicit format for a fixed process adapter.
    ///
    /// Change the generation before you change this value.
    pub fn set_process_input_format(&mut self, input_format: BrokeredInputFormat) {
        self.process_input_format = input_format;
    }

    /// Headers may contain credentials; debug output and errors never include
    /// their values.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.endpoints[0].headers_mut()
    }

    pub fn add_alternate(&mut self, endpoint: HttpTsEndpoint) {
        self.endpoints.push(endpoint);
    }

    pub fn set_startup_timeout(&mut self, timeout: Duration) {
        self.startup_timeout = timeout;
    }

    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.read_timeout = timeout;
    }

    pub fn set_viewer_batch_packets(&mut self, packets: NonZeroUsize) {
        self.viewer_batch_packets = packets;
    }

    pub fn set_recovery_policy(&mut self, recovery: RecoveryPolicy) {
        self.recovery = recovery;
    }
}

impl PartialEq for HttpTsSourceSpec {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.channel_name == other.channel_name
            && self.endpoints == other.endpoints
            && self.adapter_policy == other.adapter_policy
            && self.process_input_format == other.process_input_format
            && self.ring == other.ring
            && self.startup_timeout == other.startup_timeout
            && self.read_timeout == other.read_timeout
            && self.viewer_batch_packets == other.viewer_batch_packets
            && self.recovery == other.recovery
    }
}

impl Eq for HttpTsSourceSpec {}

/// Buffers arbitrary HTTP body chunks and emits whole 188-byte packets.
#[derive(Debug, Default)]
pub struct MpegTsPacketizer {
    pending: BytesMut,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PacketizerError {
    #[error("MPEG-TS packet {packet_index} has sync byte {actual:#04x}, expected 0x47")]
    InvalidSyncByte { packet_index: usize, actual: u8 },
    #[error("HTTP stream ended with {bytes} bytes of an incomplete MPEG-TS packet")]
    IncompleteTail { bytes: usize },
}

impl MpegTsPacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one arbitrary body chunk and returns all newly completed packets.
    ///
    /// # Errors
    ///
    /// Returns [`PacketizerError::InvalidSyncByte`] if a completed packet does
    /// not begin with the MPEG-TS sync byte.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Bytes, PacketizerError> {
        self.pending.extend_from_slice(chunk);
        let aligned_len = self.pending.len() / MPEG_TS_PACKET_SIZE * MPEG_TS_PACKET_SIZE;
        if aligned_len == 0 {
            return Ok(Bytes::new());
        }

        for (packet_index, packet) in self.pending[..aligned_len]
            .chunks_exact(MPEG_TS_PACKET_SIZE)
            .enumerate()
        {
            if packet[0] != 0x47 {
                return Err(PacketizerError::InvalidSyncByte {
                    packet_index,
                    actual: packet[0],
                });
            }
        }
        Ok(self.pending.split_to(aligned_len).freeze())
    }

    /// # Errors
    ///
    /// Returns [`PacketizerError::IncompleteTail`] if the HTTP body ends in the
    /// middle of a transport packet.
    pub fn finish(self) -> Result<(), PacketizerError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(PacketizerError::IncompleteTail {
                bytes: self.pending.len(),
            })
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending.len()
    }
}

/// Starts and shares native HTTP MPEG-TS sessions.
#[derive(Clone)]
pub struct HttpTsSessionManager {
    inner: Arc<ManagerInner>,
}

impl fmt::Debug for HttpTsSessionManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsSessionManager")
            .field("provider_count", &self.inner.providers.len())
            .field("registry_entries", &self.inner.sessions.entry_count())
            .finish_non_exhaustive()
    }
}

struct ManagerInner {
    client: Client,
    providers: Arc<DashMap<Arc<str>, ProviderSlotBroker>>,
    sessions: SharedSessionRegistry<HttpTsSessionKey, HttpTsSession, SessionStartError>,
    session_index: DashMap<HttpTsSessionKey, Weak<HttpTsSession>>,
}

impl HttpTsSessionManager {
    pub fn new(client: Client) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                client,
                providers: Arc::new(DashMap::new()),
                sessions: SharedSessionRegistry::new(),
                session_index: DashMap::new(),
            }),
        }
    }

    /// Adds a provider pool or changes its cap without revoking live sessions.
    pub fn configure_provider(&self, provider: ProviderSpec) {
        match self.inner.providers.entry(Arc::clone(&provider.pool_id)) {
            Entry::Occupied(existing) => existing.get().set_capacity(provider.max_connections),
            Entry::Vacant(vacant) => {
                vacant.insert(ProviderSlotBroker::new(
                    provider.pool_id,
                    provider.max_connections,
                ));
            }
        }
    }

    pub fn provider_snapshot(&self, pool_id: &str) -> Option<PoolSnapshot> {
        self.inner
            .providers
            .get(pool_id)
            .map(|broker| broker.snapshot())
    }

    /// Reserve a provider slot for a caller outside this process.
    ///
    /// The returned lease stays active until the caller drops it. This method
    /// lets a worker coordinate probe capacity with core playback sessions.
    ///
    /// # Errors
    ///
    /// Returns [`AcquireError::AtCapacity`] when no provider slot is available
    /// or [`AcquireError::LeaseIdExhausted`] when the broker cannot issue an id.
    ///
    /// # Panics
    ///
    /// Panics only if the provider disappears between configuration and lookup.
    pub fn reserve_provider_slot(
        &self,
        pool_id: impl Into<Arc<str>>,
        capacity: usize,
        reservation_key: impl Into<Arc<str>>,
    ) -> Result<crate::SlotLease, AcquireError> {
        let pool_id = pool_id.into();
        self.configure_provider(ProviderSpec::new(Arc::clone(&pool_id), capacity));
        let broker = self
            .inner
            .providers
            .get(&pool_id)
            .expect("provider configured before reservation");
        broker.try_acquire(reservation_key)
    }

    /// Lists live shared sessions without disclosing endpoint URLs or headers.
    pub fn list_snapshots(&self) -> Vec<HttpTsSessionSnapshot> {
        self.inner
            .session_index
            .retain(|_, session| session.strong_count() > 0);
        let mut snapshots: Vec<_> = self
            .inner
            .session_index
            .iter()
            .filter_map(|session| session.value().upgrade())
            .map(|session| session.snapshot())
            .collect();
        snapshots.sort_by(|left, right| {
            left.key
                .provider_pool_id
                .cmp(&right.key.provider_pool_id)
                .then_with(|| left.key.source_id.cmp(&right.key.source_id))
                .then_with(|| left.key.generation.cmp(&right.key.generation))
        });
        snapshots
    }

    /// List active adapter diagnostics without endpoint or credential data.
    pub fn list_adapter_diagnostics(&self) -> Vec<SessionAdapterDiagnostics> {
        self.inner
            .session_index
            .retain(|_, session| session.strong_count() > 0);
        let mut diagnostics: Vec<_> = self
            .inner
            .session_index
            .iter()
            .filter_map(|session| session.value().upgrade())
            .map(|session| session.adapter_diagnostics())
            .collect();
        diagnostics.sort_by(|left, right| {
            left.key
                .provider_pool_id
                .cmp(&right.key.provider_pool_id)
                .then_with(|| left.key.source_id.cmp(&right.key.source_id))
                .then_with(|| left.key.generation.cmp(&right.key.generation))
        });
        diagnostics
    }

    /// Closes a live session and wakes all viewers so their handles release.
    pub fn terminate(&self, key: &HttpTsSessionKey) -> bool {
        let Some(session) = self
            .inner
            .session_index
            .get(key)
            .and_then(|entry| entry.value().upgrade())
        else {
            return false;
        };
        session.terminate();
        self.inner.session_index.remove(key);
        true
    }

    /// Opens an independent viewer cursor on a single-flight shared session.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is unknown/full, response headers do
    /// not arrive before the startup timeout, or the HTTP request/status fails.
    pub async fn open(&self, source: HttpTsSourceSpec) -> Result<ViewerHandle, SessionStartError> {
        for endpoint in &source.endpoints {
            let pool_id = endpoint.effective_pool_id(&source.key.provider_pool_id);
            if !self.inner.providers.contains_key(pool_id) {
                return Err(SessionStartError::UnknownProvider {
                    pool_id: Arc::clone(pool_id),
                });
            }
        }
        let key = source.key.clone();
        let index_key = key.clone();
        let client = self.inner.client.clone();
        let providers = Arc::clone(&self.inner.providers);
        let session = self
            .inner
            .sessions
            .get_or_try_init(key, || async move {
                start_http_ts_session(client, providers, source).await
            })
            .await?;
        self.inner
            .session_index
            .insert(index_key, Arc::downgrade(&session));

        Ok(ViewerHandle::new(session))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionStartError {
    #[error("provider pool {pool_id:?} is not configured")]
    UnknownProvider { pool_id: Arc<str> },
    #[error(transparent)]
    Provider(#[from] AcquireError),
    #[error("upstream did not return response headers before the startup timeout")]
    StartupTimeout,
    #[error("upstream HTTP request failed: {message}")]
    Http { message: Arc<str> },
    #[error("the selected {adapter:?} adapter could not start")]
    Process { adapter: SessionInputAdapter },
    #[error("an HLS broker input requires FFmpeg or VLC")]
    ProcessInputRequiresProcessAdapter,
}

struct HttpTsSession {
    key: HttpTsSessionKey,
    ring: MpegTsRing,
    lease_id: u64,
    input: SessionInput,
    viewer_batch_packets: NonZeroUsize,
    diagnostics: Arc<SessionDiagnostics>,
}

impl fmt::Debug for HttpTsSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsSession")
            .field("key", &self.key)
            .field("lease_id", &self.lease_id)
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl HttpTsSession {
    fn snapshot(&self) -> HttpTsSessionSnapshot {
        self.diagnostics.snapshot(self.ring.snapshot())
    }

    fn adapter_diagnostics(&self) -> SessionAdapterDiagnostics {
        SessionAdapterDiagnostics {
            key: self.key.clone(),
            adapter: self.input.adapter(),
            process_id: self.input.process_id(),
        }
    }

    fn terminate(&self) {
        self.diagnostics.set_state(SessionState::Stopping);
        self.ring.close(RingCloseReason::Shutdown);
        info!(
            provider_pool_id = %self.key.provider_pool_id,
            source_id = %self.key.source_id,
            generation = self.key.generation,
            "media session terminated by operator"
        );
    }
}

impl Drop for HttpTsSession {
    fn drop(&mut self) {
        self.diagnostics.set_state(SessionState::Stopping);
        self.ring.close(RingCloseReason::Shutdown);
        self.input.request_shutdown();
    }
}

enum SessionInput {
    Native { task: AbortHandle },
    Process(crate::LeasedBrokeredProcessInputSession),
}

impl SessionInput {
    fn adapter(&self) -> SessionInputAdapter {
        match self {
            Self::Native { .. } => SessionInputAdapter::NativeTs,
            Self::Process(session) => match session.adapter() {
                ProcessAdapterKind::Ffmpeg => SessionInputAdapter::Ffmpeg,
                ProcessAdapterKind::Vlc => SessionInputAdapter::Vlc,
            },
        }
    }

    fn process_id(&self) -> Option<u32> {
        match self {
            Self::Native { .. } => None,
            Self::Process(session) => session.process_id(),
        }
    }

    fn request_shutdown(&mut self) {
        match self {
            Self::Native { task } => task.abort(),
            Self::Process(session) => session.request_shutdown(),
        }
    }
}

type HttpBody = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>;

struct PumpResources {
    // Rust drops struct fields in declaration order. The body—and therefore
    // response/socket adapter—is gone before provider accounting is released.
    body: Option<HttpBody>,
    lease: SlotLease,
}

struct PreparedHttpTsEndpoint {
    provider_pool_id: Arc<str>,
    url: Arc<str>,
    headers: HeaderMap,
}

async fn start_http_ts_session(
    client: Client,
    providers: Arc<DashMap<Arc<str>, ProviderSlotBroker>>,
    source: HttpTsSourceSpec,
) -> Result<HttpTsSession, SessionStartError> {
    let diagnostics = SessionDiagnostics::new(source.key.clone(), source.channel_name.clone());
    let selected_adapter = source.adapter_policy.selected_adapter();
    diagnostics.set_adapter(selected_adapter);
    info!(
        provider_pool_id = %source.key.provider_pool_id,
        source_id = %source.key.source_id,
        generation = source.key.generation,
        channel_name = source.channel_name.as_deref().unwrap_or(""),
        adapter = ?selected_adapter,
        "media session starting"
    );
    diagnostics.set_state(SessionState::Reserving);
    let primary_pool_id = source.endpoints[0]
        .effective_pool_id(&source.key.provider_pool_id)
        .clone();
    let broker = providers
        .get(&primary_pool_id)
        .map(|broker| broker.clone())
        .ok_or_else(|| SessionStartError::UnknownProvider {
            pool_id: Arc::clone(&primary_pool_id),
        })?;
    let lease = broker
        .acquire_wait(source.key.allocation_key(), source.startup_timeout)
        .await?;
    let lease_id = lease.lease_id();
    diagnostics.set_state(SessionState::Starting);

    match source.adapter_policy.selected_adapter() {
        SessionInputAdapter::NativeTs
            if !matches!(
                source.process_input_format,
                BrokeredInputFormat::DirectMpegTs
            ) =>
        {
            Err(SessionStartError::ProcessInputRequiresProcessAdapter)
        }
        SessionInputAdapter::NativeTs => {
            start_native_http_ts_session(client, providers, source, lease, lease_id, diagnostics)
                .await
        }
        SessionInputAdapter::Ffmpeg | SessionInputAdapter::Vlc => {
            start_process_http_ts_session(client, source, lease, lease_id, diagnostics).await
        }
    }
}

async fn start_native_http_ts_session(
    client: Client,
    providers: Arc<DashMap<Arc<str>, ProviderSlotBroker>>,
    source: HttpTsSourceSpec,
    lease: SlotLease,
    lease_id: u64,
    diagnostics: Arc<SessionDiagnostics>,
) -> Result<HttpTsSession, SessionStartError> {
    let primary = &source.endpoints[0];
    let response = tokio::time::timeout(
        source.startup_timeout,
        client
            .get(primary.url.as_ref())
            .headers(primary.headers.clone())
            .send(),
    )
    .await
    .map_err(|_| SessionStartError::StartupTimeout)?
    .map_err(sanitize_reqwest_error)?
    .error_for_status()
    .map_err(sanitize_reqwest_error)?;

    let ring = MpegTsRing::new(source.ring);
    let endpoints = source
        .endpoints
        .into_iter()
        .map(|endpoint| PreparedHttpTsEndpoint {
            provider_pool_id: endpoint
                .provider_pool_id
                .unwrap_or_else(|| Arc::clone(&source.key.provider_pool_id)),
            url: endpoint.url,
            headers: endpoint.headers,
        })
        .collect();
    let pump_config = HttpPumpConfig {
        client,
        providers,
        allocation_source_id: Arc::clone(&source.key.source_id),
        allocation_generation: source.key.generation,
        endpoints,
        diagnostics: Arc::clone(&diagnostics),
        recovery: source.recovery,
        startup_timeout: source.startup_timeout,
        read_timeout: source.read_timeout,
    };
    let task = tokio::spawn(pump_http_ts(response, ring.clone(), lease, pump_config));
    let abort_handle = task.abort_handle();
    drop(task);

    Ok(HttpTsSession {
        key: source.key,
        ring,
        lease_id,
        input: SessionInput::Native { task: abort_handle },
        viewer_batch_packets: source.viewer_batch_packets,
        diagnostics,
    })
}

async fn start_process_http_ts_session(
    client: Client,
    source: HttpTsSourceSpec,
    lease: SlotLease,
    lease_id: u64,
    diagnostics: Arc<SessionDiagnostics>,
) -> Result<HttpTsSession, SessionStartError> {
    let adapter = source.adapter_policy.selected_adapter();
    let primary = &source.endpoints[0];
    let mut endpoint = crate::CredentialBrokerEndpoint::new(Arc::clone(&primary.url));
    *endpoint.headers_mut() = primary.headers.clone();
    let process_adapter = match adapter {
        SessionInputAdapter::Ffmpeg => ProcessAdapterKind::Ffmpeg,
        SessionInputAdapter::Vlc => ProcessAdapterKind::Vlc,
        SessionInputAdapter::NativeTs => unreachable!("native adapter uses the HTTP session"),
    };
    let process = process_adapter
        .start_brokered_leased_with_format(
            client,
            endpoint,
            source.process_input_format,
            source.ring,
            lease,
        )
        .await
        .map_err(|_| SessionStartError::Process { adapter })?;
    diagnostics.set_state(SessionState::Priming);
    let ring = process.ring();
    monitor_process_input(ring.clone(), Arc::clone(&diagnostics));

    Ok(HttpTsSession {
        key: source.key,
        ring,
        lease_id,
        input: SessionInput::Process(process),
        viewer_batch_packets: source.viewer_batch_packets,
        diagnostics,
    })
}

fn monitor_process_input(ring: MpegTsRing, diagnostics: Arc<SessionDiagnostics>) {
    tokio::spawn(async move {
        let mut cursor = ring.subscribe_at_live_edge();
        let mut previous_first_sequence = 0_u64;
        loop {
            match cursor.next(1).await {
                Ok(RingRead::Packets { .. }) => {
                    let snapshot = ring.snapshot();
                    diagnostics.set_state(SessionState::Streaming);
                    let overwritten = snapshot
                        .first_sequence
                        .saturating_sub(previous_first_sequence);
                    diagnostics.record_write(overwritten);
                    previous_first_sequence = snapshot.first_sequence;
                }
                Ok(RingRead::Lagged { .. } | RingRead::GenerationBoundary { .. }) => {}
                Ok(RingRead::Closed {
                    reason: RingCloseReason::Shutdown,
                    ..
                })
                | Err(_) => return,
                Ok(RingRead::Closed {
                    reason: RingCloseReason::EndOfStream,
                    ..
                }) => {
                    diagnostics.record_failure(SessionFailureKind::UpstreamEnded);
                    diagnostics.set_state(SessionState::Failed);
                    return;
                }
                Ok(RingRead::Closed {
                    reason:
                        RingCloseReason::UpstreamError(_) | RingCloseReason::RecoveryExpired { .. },
                    ..
                }) => {
                    diagnostics.record_failure(SessionFailureKind::Http);
                    diagnostics.set_state(SessionState::Failed);
                    return;
                }
            }
        }
    });
}

fn sanitize_reqwest_error(error: reqwest::Error) -> SessionStartError {
    SessionStartError::Http {
        message: error.without_url().to_string().into(),
    }
}

#[allow(clippy::too_many_lines)]
async fn pump_http_ts(
    response: Response,
    ring: MpegTsRing,
    lease: SlotLease,
    config: HttpPumpConfig,
) {
    let HttpPumpConfig {
        client,
        providers,
        allocation_source_id,
        allocation_generation,
        endpoints,
        diagnostics,
        recovery,
        startup_timeout,
        read_timeout,
    } = config;
    let mut resources = PumpResources {
        body: Some(Box::pin(response.bytes_stream())),
        lease,
    };
    let mut packetizer = MpegTsPacketizer::new();
    diagnostics.set_state(SessionState::Priming);
    let initial_deadline = Instant::now() + recovery.priming_timeout();
    let initial_prime = prime_body(
        resources
            .body
            .as_mut()
            .expect("initial response body exists"),
        &mut packetizer,
        initial_deadline,
    )
    .await;

    let mut current_endpoint = 0;
    let mut active_failure = match initial_prime {
        Ok(packets) => {
            if push_packets(&ring, &diagnostics, &packets).is_err() {
                SessionFailureKind::RingClosed
            } else {
                diagnostics.set_state(SessionState::Streaming);
                stream_until_failure(
                    resources
                        .body
                        .as_mut()
                        .expect("primed response body exists"),
                    &ring,
                    &diagnostics,
                    &mut packetizer,
                    read_timeout,
                )
                .await
            }
        }
        Err(failure) => failure,
    };

    loop {
        diagnostics.record_failure(active_failure);
        warn!(
            provider_pool_id = %diagnostics.key().provider_pool_id,
            source_id = %diagnostics.key().source_id,
            generation = diagnostics.key().generation,
            channel_name = diagnostics.channel_name().unwrap_or(""),
            adapter = ?diagnostics.adapter(),
            failure = ?active_failure,
            viewers = diagnostics.has_viewers(),
            "media session upstream failure"
        );
        if diagnostics.is_stopping() {
            info!(
                provider_pool_id = %diagnostics.key().provider_pool_id,
                source_id = %diagnostics.key().source_id,
                generation = diagnostics.key().generation,
                terminal = "operator-stop",
                "media session reached terminal state"
            );
            return;
        }
        if !diagnostics.has_viewers() {
            diagnostics.set_state(SessionState::Stopping);
            ring.close(RingCloseReason::Shutdown);
            info!(
                provider_pool_id = %diagnostics.key().provider_pool_id,
                source_id = %diagnostics.key().source_id,
                generation = diagnostics.key().generation,
                terminal = "viewer-drained",
                "media session reached terminal state"
            );
            return;
        }
        resources.body.take();
        let Some(recovered) = recover_session(
            RecoveryContext {
                client: &client,
                providers: &providers,
                allocation_source_id: &allocation_source_id,
                allocation_generation,
                endpoints: &endpoints,
                ring: &ring,
                diagnostics: &diagnostics,
                recovery,
                startup_timeout,
            },
            current_endpoint,
            resources.lease.pool_id(),
        )
        .await
        else {
            diagnostics.record_failure(SessionFailureKind::RecoveryExpired);
            diagnostics.set_state(SessionState::Failed);
            warn!(
                provider_pool_id = %diagnostics.key().provider_pool_id,
                source_id = %diagnostics.key().source_id,
                generation = diagnostics.key().generation,
                attempts = diagnostics.reconnect_attempts(),
                terminal = "recovery-expired",
                "media session reached terminal state"
            );
            ring.close(RingCloseReason::RecoveryExpired {
                attempts: diagnostics.reconnect_attempts(),
            });
            return;
        };

        current_endpoint = recovered.endpoint_index;
        packetizer = recovered.packetizer;
        if let Some(lease) = recovered.lease {
            resources.lease = lease;
        }
        resources.body = Some(recovered.body);
        active_failure = stream_until_failure(
            resources.body.as_mut().expect("recovered body exists"),
            &ring,
            &diagnostics,
            &mut packetizer,
            read_timeout,
        )
        .await;
    }
}

struct HttpPumpConfig {
    client: Client,
    providers: Arc<DashMap<Arc<str>, ProviderSlotBroker>>,
    allocation_source_id: Arc<str>,
    allocation_generation: u64,
    endpoints: Vec<PreparedHttpTsEndpoint>,
    diagnostics: Arc<SessionDiagnostics>,
    recovery: RecoveryPolicy,
    startup_timeout: Duration,
    read_timeout: Duration,
}

struct RecoveredStream {
    endpoint_index: usize,
    body: HttpBody,
    packetizer: MpegTsPacketizer,
    lease: Option<SlotLease>,
}

struct RecoveryContext<'a> {
    client: &'a Client,
    providers: &'a DashMap<Arc<str>, ProviderSlotBroker>,
    allocation_source_id: &'a Arc<str>,
    allocation_generation: u64,
    endpoints: &'a [PreparedHttpTsEndpoint],
    ring: &'a MpegTsRing,
    diagnostics: &'a Arc<SessionDiagnostics>,
    recovery: RecoveryPolicy,
    startup_timeout: Duration,
}

fn acquire_recovery_lease(
    providers: &DashMap<Arc<str>, ProviderSlotBroker>,
    allocation_source_id: &Arc<str>,
    allocation_generation: u64,
    endpoint: &PreparedHttpTsEndpoint,
    current_pool_id: &str,
) -> Result<Option<SlotLease>, ()> {
    if endpoint.provider_pool_id.as_ref() == current_pool_id {
        return Ok(None);
    }
    let broker = providers
        .get(&endpoint.provider_pool_id)
        .map(|broker| broker.clone())
        .ok_or(())?;
    let allocation_key: Arc<str> = format!(
        "{}\u{1f}{}\u{1f}{}",
        endpoint.provider_pool_id, allocation_source_id, allocation_generation
    )
    .into();
    broker.try_acquire(allocation_key).map(Some).map_err(|_| ())
}

#[allow(clippy::too_many_lines)]
async fn recover_session(
    context: RecoveryContext<'_>,
    previous_endpoint: usize,
    current_pool_id: &str,
) -> Option<RecoveredStream> {
    let RecoveryContext {
        client,
        providers,
        allocation_source_id,
        allocation_generation,
        endpoints,
        ring,
        diagnostics,
        recovery,
        startup_timeout,
    } = context;
    if recovery.max_window().is_zero() {
        return None;
    }

    let deadline = Instant::now() + recovery.max_window();
    let drain_snapshot = ring.snapshot();
    let mut keepalive = RecoveryKeepalive::start(
        ring.clone(),
        Arc::clone(diagnostics),
        drain_snapshot.generation,
        drain_snapshot.next_sequence,
        recovery.keepalive_interval(),
    );
    let mut attempt = 0_usize;

    while Instant::now() < deadline {
        if !diagnostics.has_viewers() {
            return None;
        }
        let failover_offset = usize::from(endpoints.len() > 1);
        let endpoint_index = (previous_endpoint + failover_offset + attempt) % endpoints.len();
        let is_failover = endpoint_index != previous_endpoint;
        debug!(
            provider_pool_id = %diagnostics.key().provider_pool_id,
            source_id = %diagnostics.key().source_id,
            generation = diagnostics.key().generation,
            channel_name = diagnostics.channel_name().unwrap_or(""),
            adapter = ?diagnostics.adapter(),
            attempt,
            endpoint_index,
            failover = is_failover,
            "media session recovery attempt"
        );
        diagnostics.set_state(if is_failover {
            SessionState::FailingOver
        } else {
            SessionState::Recovering
        });
        diagnostics.record_reconnect(is_failover);

        let endpoint = &endpoints[endpoint_index];
        let Ok(candidate_lease) = acquire_recovery_lease(
            providers,
            allocation_source_id,
            allocation_generation,
            endpoint,
            current_pool_id,
        ) else {
            debug!(
                provider_pool_id = %diagnostics.key().provider_pool_id,
                source_id = %diagnostics.key().source_id,
                generation = diagnostics.key().generation,
                adapter = ?diagnostics.adapter(),
                attempt,
                "media session recovery attempt could not acquire provider capacity"
            );
            attempt = attempt.saturating_add(1);
            sleep_until_retry(deadline, recovery.retry_delay()).await;
            continue;
        };

        let request_deadline = deadline.min(Instant::now() + startup_timeout);
        let response = tokio::time::timeout_at(
            request_deadline,
            client
                .get(endpoint.url.as_ref())
                .headers(endpoint.headers.clone())
                .send(),
        )
        .await;
        let Ok(response) = validate_recovery_response(response) else {
            diagnostics.record_failure(SessionFailureKind::Http);
            attempt = attempt.saturating_add(1);
            sleep_until_retry(deadline, recovery.retry_delay()).await;
            continue;
        };

        diagnostics.set_state(SessionState::Priming);
        let mut body: HttpBody = Box::pin(response.bytes_stream());
        let mut packetizer = MpegTsPacketizer::new();
        let prime_deadline = deadline.min(Instant::now() + recovery.priming_timeout());
        let primed = match prime_body(&mut body, &mut packetizer, prime_deadline).await {
            Ok(packets) => packets,
            Err(failure) => {
                diagnostics.record_failure(failure);
                attempt = attempt.saturating_add(1);
                sleep_until_retry(deadline, recovery.retry_delay()).await;
                continue;
            }
        };

        while !diagnostics.viewers_drained(drain_snapshot.generation, drain_snapshot.next_sequence)
        {
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Push one null packet before the keepalive task stops. This guarantees
        // that at least one null packet reaches faulted viewers even when the
        // keepalive interval has not ticked before recovery completes.
        push_final_keepalive_null(ring, diagnostics);

        keepalive.stop().await;
        let generation = ring.start_new_generation().ok()?;
        diagnostics.set_generation(generation);
        if push_packets(ring, diagnostics, &primed).is_err() {
            diagnostics.record_failure(SessionFailureKind::RingClosed);
            return None;
        }
        diagnostics.set_state(SessionState::Streaming);
        return Some(RecoveredStream {
            endpoint_index,
            body,
            packetizer,
            lease: candidate_lease,
        });
    }

    None
}

fn validate_recovery_response(
    response: Result<Result<Response, reqwest::Error>, tokio::time::error::Elapsed>,
) -> Result<Response, ()> {
    response
        .map_err(|_| ())?
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())
}

async fn sleep_until_retry(deadline: Instant, delay: Duration) {
    let until = deadline.min(Instant::now() + delay);
    tokio::time::sleep_until(until).await;
}

async fn prime_body(
    body: &mut HttpBody,
    packetizer: &mut MpegTsPacketizer,
    deadline: Instant,
) -> Result<Bytes, SessionFailureKind> {
    let mut tracker = PatPmtTracker::default();
    let mut retained = BytesMut::new();
    let mut first_sequence = 0_u64;
    let mut next_sequence = 0_u64;
    loop {
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .map_err(|_| SessionFailureKind::ReadTimeout)?
            .ok_or(SessionFailureKind::UpstreamEnded)?
            .map_err(|_| SessionFailureKind::UpstreamRead)?;
        let packets = packetizer
            .push(&chunk)
            .map_err(|error| packetizer_failure(&error))?;
        for (packet_index, packet) in packets.chunks_exact(MPEG_TS_PACKET_SIZE).enumerate() {
            if retained.len() == MAX_PRIMING_PACKETS * MPEG_TS_PACKET_SIZE {
                retained.advance(MPEG_TS_PACKET_SIZE);
                first_sequence = first_sequence.saturating_add(1);
                if tracker
                    .candidate_start()
                    .is_some_and(|candidate| candidate < first_sequence)
                {
                    tracker.reset();
                }
            }
            retained.extend_from_slice(packet);
            if let Some(boundary) = tracker.observe(packet, next_sequence) {
                let offset_packets = boundary
                    .checked_sub(first_sequence)
                    .ok_or(SessionFailureKind::Priming)?;
                let offset = usize::try_from(offset_packets)
                    .ok()
                    .and_then(|packets| packets.checked_mul(MPEG_TS_PACKET_SIZE))
                    .filter(|offset| *offset < retained.len())
                    .ok_or(SessionFailureKind::Priming)?;
                let remaining_offset = (packet_index + 1) * MPEG_TS_PACKET_SIZE;
                retained.extend_from_slice(&packets[remaining_offset..]);
                return Ok(retained.freeze().slice(offset..));
            }
            next_sequence = next_sequence.saturating_add(1);
        }
    }
}

async fn stream_until_failure(
    body: &mut HttpBody,
    ring: &MpegTsRing,
    diagnostics: &SessionDiagnostics,
    packetizer: &mut MpegTsPacketizer,
    read_timeout: Duration,
) -> SessionFailureKind {
    loop {
        match tokio::time::timeout(read_timeout, body.next()).await {
            Ok(Some(Ok(chunk))) => match packetizer.push(&chunk) {
                Ok(packets) => {
                    if push_packets(ring, diagnostics, &packets).is_err() {
                        debug!(
                            provider_pool_id = %diagnostics.key().provider_pool_id,
                            source_id = %diagnostics.key().source_id,
                            generation = diagnostics.key().generation,
                            channel_name = diagnostics.channel_name().unwrap_or(""),
                            adapter = ?diagnostics.adapter(),
                            "media session ring closed while publishing packets"
                        );
                        return SessionFailureKind::RingClosed;
                    }
                }
                Err(error) => {
                    warn!(
                        provider_pool_id = %diagnostics.key().provider_pool_id,
                        source_id = %diagnostics.key().source_id,
                        generation = diagnostics.key().generation,
                        channel_name = diagnostics.channel_name().unwrap_or(""),
                        adapter = ?diagnostics.adapter(),
                        error = %error,
                        "media session rejected MPEG-TS input"
                    );
                    return packetizer_failure(&error);
                }
            },
            Ok(Some(Err(error))) => {
                warn!(
                    provider_pool_id = %diagnostics.key().provider_pool_id,
                    source_id = %diagnostics.key().source_id,
                    generation = diagnostics.key().generation,
                    channel_name = diagnostics.channel_name().unwrap_or(""),
                    adapter = ?diagnostics.adapter(),
                    error = %error.without_url(),
                    "media session upstream read failed"
                );
                return SessionFailureKind::UpstreamRead;
            }
            Err(_) => {
                warn!(
                    provider_pool_id = %diagnostics.key().provider_pool_id,
                    source_id = %diagnostics.key().source_id,
                    generation = diagnostics.key().generation,
                    channel_name = diagnostics.channel_name().unwrap_or(""),
                    adapter = ?diagnostics.adapter(),
                    timeout_ms = u64::try_from(read_timeout.as_millis()).unwrap_or(u64::MAX),
                    "media session upstream read timed out"
                );
                return SessionFailureKind::ReadTimeout;
            }
            Ok(None) => {
                return if packetizer.pending_bytes() == 0 {
                    SessionFailureKind::UpstreamEnded
                } else {
                    warn!(
                        provider_pool_id = %diagnostics.key().provider_pool_id,
                        source_id = %diagnostics.key().source_id,
                        generation = diagnostics.key().generation,
                        channel_name = diagnostics.channel_name().unwrap_or(""),
                        adapter = ?diagnostics.adapter(),
                        pending_bytes = packetizer.pending_bytes(),
                        "media session upstream ended with an incomplete MPEG-TS packet"
                    );
                    SessionFailureKind::IncompleteTail
                };
            }
        }
    }
}

fn push_packets(
    ring: &MpegTsRing,
    diagnostics: &SessionDiagnostics,
    packets: &[u8],
) -> Result<(), RingWriteError> {
    if packets.is_empty() {
        return Ok(());
    }
    let outcome = ring.push(packets)?;
    diagnostics.record_write(outcome.packets_overwritten);
    Ok(())
}

const fn packetizer_failure(error: &PacketizerError) -> SessionFailureKind {
    match error {
        PacketizerError::InvalidSyncByte { .. } => SessionFailureKind::InvalidSync,
        PacketizerError::IncompleteTail { .. } => SessionFailureKind::IncompleteTail,
    }
}

struct RecoveryKeepalive {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RecoveryKeepalive {
    fn start(
        ring: MpegTsRing,
        diagnostics: Arc<SessionDiagnostics>,
        generation: u64,
        drain_target: u64,
        interval: Duration,
    ) -> Self {
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;
            let mut continuity = 0_u8;
            loop {
                ticker.tick().await;
                if diagnostics.viewers_drained(generation, drain_target) {
                    let packet = null_packet(continuity);
                    continuity = (continuity + 1) & 0x0f;
                    let Ok(outcome) = ring.push(&packet) else {
                        return;
                    };
                    diagnostics.record_write(outcome.packets_overwritten);
                }
            }
        });
        Self { task: Some(task) }
    }

    async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for RecoveryKeepalive {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn null_packet(continuity_counter: u8) -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = 0x1f;
    packet[2] = 0xff;
    packet[3] = 0x10 | (continuity_counter & 0x0f);
    packet
}

/// Pushes one null packet so faulted viewers receive a keepalive before the
/// recovery keepalive task stops.
fn push_final_keepalive_null(ring: &MpegTsRing, diagnostics: &SessionDiagnostics) {
    let packet = null_packet(0);
    if ring.push(&packet).is_ok() {
        diagnostics.record_write(0);
    }
}

/// One viewer's independent cursor and strong reference to a shared session.
pub struct ViewerHandle {
    session: Arc<HttpTsSession>,
    cursor: crate::RingCursor,
    registration: ViewerRegistration,
}

impl fmt::Debug for ViewerHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewerHandle")
            .field("key", &self.session.key)
            .field("lease_id", &self.session.lease_id)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl ViewerHandle {
    fn new(session: Arc<HttpTsSession>) -> Self {
        let cursor = session.ring.subscribe();
        let registration = session
            .diagnostics
            .register_viewer(cursor.generation(), cursor.next_sequence());
        Self {
            session,
            cursor,
            registration,
        }
    }

    /// Adds another independently positioned viewer to this shared session.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self::new(Arc::clone(&self.session))
    }

    pub fn session_key(&self) -> &HttpTsSessionKey {
        &self.session.key
    }

    pub fn lease_id(&self) -> u64 {
        self.session.lease_id
    }

    pub fn input_adapter(&self) -> SessionInputAdapter {
        self.session.input.adapter()
    }

    pub fn ring_snapshot(&self) -> crate::RingSnapshot {
        self.session.ring.snapshot()
    }

    pub fn session_snapshot(&self) -> HttpTsSessionSnapshot {
        self.session.snapshot()
    }

    /// Converts this viewer into a `Send + 'static` byte stream accepted by
    /// `axum::body::Body::from_stream`.
    pub fn into_byte_stream(self) -> ViewerByteStream {
        let Self {
            session,
            mut cursor,
            registration,
        } = self;
        let max_packets = session.viewer_batch_packets.get();
        let inner = Box::pin(async_stream::try_stream! {
            // Holding the session here makes stream drop equivalent to viewer
            // drop. The final stream drop cancels the HTTP request.
            let session_guard = session;
            loop {
                match cursor.next(max_packets).await? {
                    RingRead::Packets {
                        generation, bytes, ..
                    } => {
                        registration.update(generation, cursor.next_sequence());
                        yield bytes;
                    }
                    RingRead::Lagged {
                        skipped_packets, ..
                    } => {
                        session_guard.diagnostics.record_lag();
                        registration.update(cursor.generation(), cursor.next_sequence());
                        Err(ViewerStreamError::Lagged { skipped_packets })?;
                    }
                    RingRead::GenerationBoundary {
                        generation,
                        ..
                    } => {
                        // The supervisor primes each new generation to a PAT
                        // boundary before rotating the ring. Keep the HTTP body
                        // open and resume on the next packet event.
                        registration.update(generation, cursor.next_sequence());
                    }
                    RingRead::Closed {
                        reason: RingCloseReason::EndOfStream | RingCloseReason::Shutdown,
                        ..
                    } => break,
                    RingRead::Closed {
                        reason: RingCloseReason::UpstreamError(message),
                        ..
                    } => Err(ViewerStreamError::Upstream { message })?,
                    RingRead::Closed {
                        reason: RingCloseReason::RecoveryExpired { attempts },
                        ..
                    } => Err(ViewerStreamError::RecoveryExpired { attempts })?,
                }
            }
        });
        ViewerByteStream { inner }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ViewerStreamError {
    #[error(transparent)]
    Ring(#[from] RingReadError),
    #[error("viewer fell behind the live buffer by {skipped_packets} MPEG-TS packets")]
    Lagged { skipped_packets: u64 },
    #[error("stream generation changed from {previous_generation} to {generation}")]
    GenerationChanged {
        previous_generation: u64,
        generation: u64,
    },
    #[error("upstream stream failed: {message}")]
    Upstream { message: Arc<str> },
    #[error("upstream recovery expired after {attempts} coordinated attempts")]
    RecoveryExpired { attempts: u64 },
}

/// Type-erased MPEG-TS byte stream suitable for HTTP response bodies.
pub struct ViewerByteStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, ViewerStreamError>> + Send + 'static>>,
}

impl fmt::Debug for ViewerByteStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewerByteStream")
            .finish_non_exhaustive()
    }
}

impl Stream for ViewerByteStream {
    type Item = Result<Bytes, ViewerStreamError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::psi::test_support::{TEST_PMT_PID, TEST_VIDEO_PID, pat_packet, pmt_packet};

    use super::*;

    fn packet(id: u32) -> [u8; MPEG_TS_PACKET_SIZE] {
        match id % 3 {
            0 => pat_packet(),
            1 => pmt_packet(),
            _ => labelled_packet(id, TEST_VIDEO_PID),
        }
    }

    fn labelled_packet(label: u32, pid: u16) -> [u8; MPEG_TS_PACKET_SIZE] {
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = u8::try_from((pid >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(pid & 0xff).unwrap();
        packet[3] = 0x10;
        packet[4..8].copy_from_slice(&label.to_be_bytes());
        packet
    }

    fn labelled_generation(base: u32) -> Bytes {
        Bytes::from(
            [
                pat_packet(),
                pmt_packet(),
                labelled_packet(base, TEST_VIDEO_PID),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        )
    }

    fn packet_pid(packet: &[u8]) -> u16 {
        (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
    }

    fn packet_label(packet: &[u8]) -> u32 {
        u32::from_be_bytes(packet[4..8].try_into().unwrap())
    }

    fn media_labels(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(MPEG_TS_PACKET_SIZE)
            .filter(|packet| packet_pid(packet) == TEST_VIDEO_PID)
            .map(packet_label)
            .collect()
    }

    #[test]
    fn packetizer_aligns_arbitrary_http_chunks() {
        let expected: Vec<u8> = (0..8_u32).flat_map(packet).collect();
        let chunk_sizes = [1, 2, 184, 3, 377, 11, 509, 99, 1_000];
        let mut input = expected.as_slice();
        let mut packetizer = MpegTsPacketizer::new();
        let mut actual = Vec::new();

        for size in chunk_sizes.into_iter().cycle() {
            if input.is_empty() {
                break;
            }
            let size = size.min(input.len());
            let completed = packetizer.push(&input[..size]).unwrap();
            assert_eq!(completed.len() % MPEG_TS_PACKET_SIZE, 0);
            actual.extend_from_slice(&completed);
            input = &input[size..];
        }

        packetizer.finish().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn packetizer_reports_incomplete_tail_and_bad_sync() {
        let mut incomplete = MpegTsPacketizer::new();
        incomplete.push(&packet(1)[..17]).unwrap();
        assert_eq!(
            incomplete.finish(),
            Err(PacketizerError::IncompleteTail { bytes: 17 })
        );

        let mut bad = packet(1);
        bad[0] = 0;
        assert_eq!(
            MpegTsPacketizer::new().push(&bad),
            Err(PacketizerError::InvalidSyncByte {
                packet_index: 0,
                actual: 0
            })
        );
    }

    #[test]
    fn source_debug_redacts_url_and_header_values() {
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "source", 1),
            "http://user:password@example.test/live?token=secret",
            MpegTsRingConfig::new(8, 2).unwrap(),
        );
        source.headers_mut().insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer secret"),
        );

        let debug = format!("{source:?}");
        assert!(!debug.contains("password"));
        assert!(!debug.contains("token"));
        assert!(!debug.contains("Bearer"));
        assert!(!debug.contains("secret"));
        assert!(debug.contains("authorization"));

        assert_eq!(source.key().source_id.as_ref(), "source");
        source.set_startup_timeout(Duration::from_millis(50));
        source.set_viewer_batch_packets(NonZeroUsize::new(7).unwrap());
        let mut alternate = HttpTsEndpoint::new("http://alternate.test/live?key=hidden");
        alternate.headers_mut().insert(
            reqwest::header::COOKIE,
            reqwest::header::HeaderValue::from_static("session=hidden"),
        );
        source.add_alternate(alternate);
        let cloned = source.clone();
        assert_eq!(source, cloned);
        assert_ne!(
            source,
            HttpTsSourceSpec::new(
                HttpTsSessionKey::new("provider", "different", 1),
                "http://example.test/live",
                MpegTsRingConfig::new(8, 2).unwrap(),
            )
        );
        let debug = format!("{source:?}");
        assert!(debug.contains("alternate_count"));
        assert!(!debug.contains("hidden"));
    }

    #[test]
    fn adapter_policy_selects_typed_direct_input_without_hls() {
        assert_eq!(
            InputAdapterPolicy::Auto.selected_adapter(),
            SessionInputAdapter::NativeTs
        );
        assert_eq!(
            InputAdapterPolicy::NativeTs.selected_adapter(),
            SessionInputAdapter::NativeTs
        );
        assert_eq!(
            InputAdapterPolicy::Ffmpeg.selected_adapter(),
            SessionInputAdapter::Ffmpeg
        );
        assert_eq!(
            InputAdapterPolicy::Vlc.selected_adapter(),
            SessionInputAdapter::Vlc
        );

        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "source", 1),
            "http://provider.test/live.ts",
            MpegTsRingConfig::new(8, 0).unwrap(),
        );
        assert_eq!(source.adapter_policy(), InputAdapterPolicy::Auto);
        source.set_adapter_policy(InputAdapterPolicy::Vlc);
        assert_eq!(source.adapter_policy(), InputAdapterPolicy::Vlc);
        assert!(format!("{source:?}").contains("adapter_policy"));
    }

    #[cfg(unix)]
    fn fixed_adapter_fixture() -> Vec<u8> {
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=32x32:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000",
                "-t",
                "1",
                "-c:v",
                "mpeg2video",
                "-c:a",
                "mp2",
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .output()
            .expect("the fixed FFmpeg test fixture must start");
        assert!(output.status.success(), "the fixed fixture must be valid");
        output.stdout
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_lines)]
    async fn fixed_adapter_shares_one_process_and_stops(
        policy: InputAdapterPolicy,
        expected_adapter: SessionInputAdapter,
        upstream_is_live: bool,
    ) {
        use std::{convert::Infallible, sync::Arc};

        use axum::{
            Router,
            body::Body,
            extract::State,
            http::{HeaderMap, HeaderValue, header},
            response::Response,
            routing::get,
        };
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            disconnects: Arc<AtomicUsize>,
            fixture: Arc<Vec<u8>>,
            live: bool,
        }

        struct DisconnectGuard(Arc<AtomicUsize>);

        impl Drop for DisconnectGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn upstream(State(state): State<UpstreamState>, headers: HeaderMap) -> Response {
            assert_eq!(
                headers.get(header::AUTHORIZATION),
                Some(&reqwest::header::HeaderValue::from_static(
                    "Bearer provider-secret"
                ))
            );
            state.requests.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                let _guard = DisconnectGuard(Arc::clone(&state.disconnects));
                loop {
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&state.fixture));
                    if !state.live {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            let mut response = Response::new(Body::from_stream(stream));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
            response
        }

        let state = UpstreamState {
            requests: Arc::new(AtomicUsize::new(0)),
            disconnects: Arc::new(AtomicUsize::new(0)),
            fixture: Arc::new(fixed_adapter_fixture()),
            live: upstream_is_live,
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/live.ts", get(upstream))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "channel", 1),
            format!("http://{address}/live.ts?username=provider-user&password=provider-password"),
            MpegTsRingConfig::new(8, 0).unwrap(),
        );
        source.set_adapter_policy(policy);
        source.headers_mut().insert(
            header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer provider-secret"),
        );
        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();

        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(first.input_adapter(), expected_adapter);
        assert_eq!(second.input_adapter(), expected_adapter);
        let diagnostics = manager.list_adapter_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].adapter, expected_adapter);
        assert!(diagnostics[0].process_id.is_some());
        let diagnostic_debug = format!("{diagnostics:?}");
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(!diagnostic_debug.contains(secret));
        }

        timeout(Duration::from_secs(5), async {
            loop {
                let ring = first.ring_snapshot();
                if ring.first_sequence > 0
                    && (!upstream_is_live
                        || (ring.closed.is_none()
                            && first.session_snapshot().state == SessionState::Streaming))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the fixed adapter must wrap its bounded ring without exit; requests={}, diagnostics={:?}, ring={:?}",
                state.requests.load(Ordering::SeqCst),
                manager.list_adapter_diagnostics(),
                first.ring_snapshot(),
            )
        });
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        if upstream_is_live {
            assert_eq!(
                manager
                    .provider_snapshot("provider")
                    .unwrap()
                    .active_sessions,
                1
            );
        }

        drop(first);
        if upstream_is_live {
            assert_eq!(
                manager
                    .provider_snapshot("provider")
                    .unwrap()
                    .active_sessions,
                1
            );
        }
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.disconnects.load(Ordering::SeqCst) == 1
                    && manager
                        .provider_snapshot("provider")
                        .unwrap()
                        .active_sessions
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the final viewer must stop the fixed adapter within two seconds");

        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ffmpeg_policy_shares_one_process_wraps_and_stops() {
        fixed_adapter_shares_one_process_and_stops(
            InputAdapterPolicy::Ffmpeg,
            SessionInputAdapter::Ffmpeg,
            true,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn vlc_policy_shares_one_process_and_remuxes() {
        fixed_adapter_shares_one_process_and_stops(
            InputAdapterPolicy::Vlc,
            SessionInputAdapter::Vlc,
            false,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn explicit_hls_ffmpeg_policy_shares_one_process_and_nested_resources() {
        use std::{
            collections::HashMap,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            http::{HeaderMap, HeaderValue, header},
            response::Response,
            routing::get,
        };
        use tokio::time::timeout;

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            fixture: Arc<Vec<u8>>,
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
            let body = match path.as_str() {
                "master.m3u8" => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=1\nnested/child.m3u8\n".as_bytes().to_vec(),
                "nested/child.m3u8" => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,Segment\nsegment.ts\n#EXT-X-ENDLIST\n".as_bytes().to_vec(),
                "nested/segment.ts" => state.fixture.as_ref().clone(),
                _ => Vec::new(),
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
            requests: Arc::new(Mutex::new(HashMap::new())),
            fixture: Arc::new(fixed_adapter_fixture()),
        };
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
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

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "hls-channel", 1),
            format!(
                "http://{address}/master.m3u8?username=provider-user&password=provider-password"
            ),
            MpegTsRingConfig::new(8, 0).unwrap(),
        );
        source.set_adapter_policy(InputAdapterPolicy::Ffmpeg);
        source
            .set_process_input_format(BrokeredInputFormat::Hls(crate::HlsBrokerConfig::default()));
        source.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-secret"),
        );
        let source_debug = format!("{source:?}");
        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(first.input_adapter(), SessionInputAdapter::Ffmpeg);
        assert_eq!(manager.list_adapter_diagnostics().len(), 1);
        timeout(Duration::from_secs(5), async {
            loop {
                if first.ring_snapshot().next_sequence > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "FFmpeg must read the explicit loopback HLS manifest; requests={:?}, ring={:?}",
                state.requests.lock().unwrap(),
                first.ring_snapshot(),
            )
        });
        let diagnostics = format!("{:?} {source_debug}", manager.list_adapter_diagnostics());
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(!diagnostics.contains(secret));
        }
        {
            let requests = state.requests.lock().unwrap();
            assert_eq!(requests.get("master.m3u8"), Some(&1));
            assert_eq!(requests.get("nested/child.m3u8"), Some(&1));
            assert_eq!(requests.get("nested/segment.ts"), Some(&1));
        }
        drop(first);
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .provider_snapshot("provider")
                    .unwrap()
                    .active_sessions
                    == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("final HLS viewer cleanup must release the process slot");
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn manager_value_api_and_private_pump_edges_are_covered() {
        use futures_util::stream;

        let manager = HttpTsSessionManager::new(Client::new());
        assert!(format!("{manager:?}").contains("provider_count"));
        assert!(manager.list_snapshots().is_empty());
        assert!(manager.provider_snapshot("missing").is_none());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        manager.configure_provider(ProviderSpec::new("provider", 2));
        assert_eq!(manager.provider_snapshot("provider").unwrap().capacity, 2);
        let reservation = manager
            .reserve_provider_slot("reservation-pool", 1, "worker-probe")
            .expect("configured provider accepts a reservation");
        assert_eq!(
            manager
                .provider_snapshot("reservation-pool")
                .unwrap()
                .active_sessions,
            1
        );
        drop(reservation);

        let source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("unknown", "source", 1),
            "http://127.0.0.1:1/secret?token=never-requested",
            MpegTsRingConfig::new(2, 0).unwrap(),
        );
        assert!(matches!(
            manager.open(source).await,
            Err(SessionStartError::UnknownProvider { pool_id }) if pool_id.as_ref() == "unknown"
        ));

        let diagnostics =
            SessionDiagnostics::new(HttpTsSessionKey::new("provider", "unit", 1), None);
        let ring = MpegTsRing::new(MpegTsRingConfig::new(4, 0).unwrap());

        let mut empty: HttpBody = Box::pin(stream::empty());
        assert_eq!(
            prime_body(
                &mut empty,
                &mut MpegTsPacketizer::new(),
                Instant::now() + Duration::from_millis(20),
            )
            .await,
            Err(SessionFailureKind::UpstreamEnded)
        );

        let mut invalid: HttpBody = Box::pin(stream::iter([Ok::<_, reqwest::Error>(Bytes::from(
            vec![0_u8; MPEG_TS_PACKET_SIZE],
        ))]));
        assert_eq!(
            prime_body(
                &mut invalid,
                &mut MpegTsPacketizer::new(),
                Instant::now() + Duration::from_millis(20),
            )
            .await,
            Err(SessionFailureKind::InvalidSync)
        );

        let mut no_pat: HttpBody = Box::pin(stream::iter([Ok::<_, reqwest::Error>(Bytes::from(
            labelled_packet(1, 0x100).to_vec(),
        ))]));
        assert_eq!(
            prime_body(
                &mut no_pat,
                &mut MpegTsPacketizer::new(),
                Instant::now() + Duration::from_millis(20),
            )
            .await,
            Err(SessionFailureKind::UpstreamEnded)
        );

        let mut pat_without_pmt: HttpBody = Box::pin(
            stream::iter([Ok::<_, reqwest::Error>(Bytes::copy_from_slice(
                &pat_packet(),
            ))])
            .chain(stream::pending()),
        );
        assert_eq!(
            prime_body(
                &mut pat_without_pmt,
                &mut MpegTsPacketizer::new(),
                Instant::now() + Duration::from_millis(2),
            )
            .await,
            Err(SessionFailureKind::ReadTimeout)
        );

        let prefixed = [
            labelled_packet(99, TEST_VIDEO_PID).to_vec(),
            pat_packet().to_vec(),
            pmt_packet().to_vec(),
            labelled_packet(100, TEST_VIDEO_PID).to_vec(),
        ]
        .concat();
        let mut valid_psi: HttpBody = Box::pin(stream::iter([Ok::<_, reqwest::Error>(
            Bytes::from(prefixed),
        )]));
        let primed = prime_body(
            &mut valid_psi,
            &mut MpegTsPacketizer::new(),
            Instant::now() + Duration::from_millis(20),
        )
        .await
        .unwrap();
        assert_eq!(&primed[..MPEG_TS_PACKET_SIZE], &pat_packet());
        assert_eq!(media_labels(&primed), [100]);

        let mut pending: HttpBody = Box::pin(stream::pending());
        assert_eq!(
            prime_body(
                &mut pending,
                &mut MpegTsPacketizer::new(),
                Instant::now() + Duration::from_millis(1),
            )
            .await,
            Err(SessionFailureKind::ReadTimeout)
        );

        let mut incomplete: HttpBody = Box::pin(stream::iter([Ok::<_, reqwest::Error>(
            Bytes::from_static(&[0x47]),
        )]));
        assert_eq!(
            stream_until_failure(
                &mut incomplete,
                &ring,
                &diagnostics,
                &mut MpegTsPacketizer::new(),
                Duration::from_secs(1),
            )
            .await,
            SessionFailureKind::IncompleteTail
        );

        let closed = MpegTsRing::new(MpegTsRingConfig::new(2, 0).unwrap());
        closed.close(RingCloseReason::Shutdown);
        let mut valid: HttpBody = Box::pin(stream::iter([Ok::<_, reqwest::Error>(
            labelled_generation(1),
        )]));
        assert_eq!(
            stream_until_failure(
                &mut valid,
                &closed,
                &diagnostics,
                &mut MpegTsPacketizer::new(),
                Duration::from_secs(1),
            )
            .await,
            SessionFailureKind::RingClosed
        );
        assert_eq!(push_packets(&ring, &diagnostics, &[]), Ok(()));

        assert!(
            recover_session(
                RecoveryContext {
                    client: &Client::new(),
                    providers: &DashMap::new(),
                    allocation_source_id: &Arc::from("source"),
                    allocation_generation: 1,
                    endpoints: &[],
                    ring: &ring,
                    diagnostics: &diagnostics,
                    recovery: RecoveryPolicy::disabled(),
                    startup_timeout: Duration::from_millis(10),
                },
                0,
                "provider",
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn recovery_stops_before_endpoint_selection_when_all_viewers_leave() {
        let client = Client::new();
        let providers = DashMap::new();
        let source_id: Arc<str> = "source".into();
        let diagnostics =
            SessionDiagnostics::new(HttpTsSessionKey::new("provider", "source", 1), None);
        let ring = MpegTsRing::new(MpegTsRingConfig::new(2, 0).unwrap());
        let result = recover_session(
            RecoveryContext {
                client: &client,
                providers: &providers,
                allocation_source_id: &source_id,
                allocation_generation: 1,
                endpoints: &[],
                ring: &ring,
                diagnostics: &diagnostics,
                recovery: RecoveryPolicy::default(),
                startup_timeout: Duration::from_millis(10),
            },
            0,
            "provider",
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn pump_stops_and_closes_ring_when_initial_upstream_fails_without_viewers() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 256];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
        });

        let client = Client::new();
        let response = client
            .get(format!("http://{address}/empty.ts"))
            .send()
            .await
            .unwrap();
        let broker = ProviderSlotBroker::new("provider", 1);
        let lease = broker.try_acquire("source").unwrap();
        let ring = MpegTsRing::new(MpegTsRingConfig::new(2, 0).unwrap());
        let diagnostics =
            SessionDiagnostics::new(HttpTsSessionKey::new("provider", "source", 1), None);
        let config = HttpPumpConfig {
            client: client.clone(),
            providers: Arc::new(DashMap::new()),
            allocation_source_id: Arc::from("source"),
            allocation_generation: 1,
            endpoints: Vec::new(),
            diagnostics: Arc::clone(&diagnostics),
            recovery: RecoveryPolicy::disabled(),
            startup_timeout: Duration::from_millis(10),
            read_timeout: Duration::from_millis(10),
        };

        pump_http_ts(response, ring.clone(), lease, config).await;
        assert_eq!(
            diagnostics.snapshot(ring.snapshot()).state,
            SessionState::Stopping
        );
        assert!(ring.snapshot().closed.is_some());
        server.await.unwrap();
    }

    fn local_viewer(
        capacity: usize,
        pre_roll: usize,
    ) -> (ViewerHandle, MpegTsRing, Arc<SessionDiagnostics>) {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(capacity, pre_roll).unwrap());
        let diagnostics =
            SessionDiagnostics::new(HttpTsSessionKey::new("provider", "local", 1), None);
        diagnostics.set_state(SessionState::Streaming);
        let task = tokio::spawn(std::future::pending::<()>());
        let abort_handle = task.abort_handle();
        drop(task);
        let session = Arc::new(HttpTsSession {
            key: HttpTsSessionKey::new("provider", "local", 1),
            ring: ring.clone(),
            lease_id: 7,
            input: SessionInput::Native { task: abort_handle },
            viewer_batch_packets: NonZeroUsize::new(2).unwrap(),
            diagnostics: Arc::clone(&diagnostics),
        });
        (ViewerHandle::new(session), ring, diagnostics)
    }

    #[tokio::test]
    async fn local_viewer_surfaces_lag_generation_close_and_redacted_debug() {
        let (viewer, ring, diagnostics) = local_viewer(2, 2);
        assert_eq!(viewer.session_key().source_id.as_ref(), "local");
        assert_eq!(viewer.lease_id(), 7);
        assert_eq!(viewer.session_snapshot().viewer_count, 1);
        assert!(format!("{viewer:?}").contains("lease_id"));
        let fork = viewer.fork();
        assert_eq!(fork.session_snapshot().viewer_count, 2);
        drop(fork);
        assert_eq!(viewer.session_snapshot().viewer_count, 1);

        ring.push(&(0..5_u32).flat_map(packet).collect::<Vec<_>>())
            .unwrap();
        let mut stream = viewer.into_byte_stream();
        assert!(format!("{stream:?}").contains("ViewerByteStream"));
        assert!(matches!(
            stream.next().await,
            Some(Err(ViewerStreamError::Lagged { skipped_packets: 3 }))
        ));
        assert_eq!(diagnostics.snapshot(ring.snapshot()).ring_lag_events, 1);
        drop(stream);

        let (viewer, ring, _) = local_viewer(4, 4);
        assert_eq!(ring.start_new_generation().unwrap(), 1);
        ring.push(&labelled_generation(10)).unwrap();
        let mut stream = viewer.into_byte_stream();
        let bytes = stream.next().await.unwrap().unwrap();
        assert_eq!(packet_pid(&bytes[..MPEG_TS_PACKET_SIZE]), 0);
        ring.close(RingCloseReason::EndOfStream);
        assert!(stream.next().await.unwrap().is_ok());
        assert!(stream.next().await.is_none());

        let (viewer, ring, _) = local_viewer(2, 0);
        ring.close(RingCloseReason::UpstreamError(
            "credential-free failure".into(),
        ));
        let mut stream = viewer.into_byte_stream();
        assert_eq!(
            stream.next().await,
            Some(Err(ViewerStreamError::Upstream {
                message: "credential-free failure".into(),
            }))
        );

        let (viewer, ring, _) = local_viewer(2, 0);
        ring.close(RingCloseReason::Shutdown);
        assert!(viewer.into_byte_stream().next().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn two_viewers_share_one_axum_request_and_final_drop_releases_it() {
        use std::convert::Infallible;

        use axum::{Router, body::Body, extract::State, routing::get};
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            disconnects: Arc<AtomicUsize>,
        }

        struct DisconnectGuard(UpstreamState);

        impl Drop for DisconnectGuard {
            fn drop(&mut self) {
                self.0.disconnects.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn upstream(State(state): State<UpstreamState>) -> Body {
            state.requests.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                let _guard = DisconnectGuard(state);
                let mut id = 0_u32;
                loop {
                    let packet = packet(id);
                    // Deliberately split every TS packet across HTTP body frames.
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&packet[..37]));
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&packet[37..]));
                    id = id.wrapping_add(1);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            Body::from_stream(stream)
        }

        let state = UpstreamState {
            requests: Arc::new(AtomicUsize::new(0)),
            disconnects: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/live.ts", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "channel-1", 1),
            format!("http://{address}/live.ts"),
            MpegTsRingConfig::new(32, 0).unwrap(),
        );

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1
        );

        // Exercise the exact Axum integration surface, not just the underlying
        // `Stream` implementation.
        let mut first = Body::from_stream(first.into_byte_stream()).into_data_stream();
        let mut second = second.into_byte_stream();
        let first_bytes = timeout(Duration::from_secs(1), first.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let second_bytes = timeout(Duration::from_secs(1), second.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first_bytes.len() % MPEG_TS_PACKET_SIZE, 0);
        assert_eq!(second_bytes.len() % MPEG_TS_PACKET_SIZE, 0);

        drop(first);
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1
        );
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.disconnects.load(Ordering::SeqCst) == 1
                    && manager
                        .provider_snapshot("provider")
                        .unwrap()
                        .active_sessions
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the final viewer must cancel HTTP and release its lease within two seconds");
        assert_eq!(state.disconnects.load(Ordering::SeqCst), 1);
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .high_watermark,
            1
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn three_channels_six_viewers_obey_cap_wrap_and_last_viewer_shutdown() {
        use std::{collections::HashMap, convert::Infallible, sync::Mutex};

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            routing::get,
        };
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            requests_by_channel: Arc<Mutex<HashMap<String, usize>>>,
            disconnects: Arc<AtomicUsize>,
        }

        struct DisconnectGuard(UpstreamState);

        impl Drop for DisconnectGuard {
            fn drop(&mut self) {
                self.0.disconnects.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn upstream(Path(channel): Path<String>, State(state): State<UpstreamState>) -> Body {
            state.requests.fetch_add(1, Ordering::SeqCst);
            *state
                .requests_by_channel
                .lock()
                .unwrap()
                .entry(channel)
                .or_default() += 1;
            let stream = async_stream::stream! {
                let _guard = DisconnectGuard(state);
                let mut id = 0_u32;
                loop {
                    let packet = packet(id);
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&packet[..71]));
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&packet[71..]));
                    id = id.wrapping_add(1);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            };
            Body::from_stream(stream)
        }

        let state = UpstreamState {
            requests: Arc::new(AtomicUsize::new(0)),
            requests_by_channel: Arc::new(Mutex::new(HashMap::new())),
            disconnects: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/{channel}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 3));
        let source = |channel: &str| {
            HttpTsSourceSpec::new(
                HttpTsSessionKey::new("provider", channel.to_owned(), 1),
                format!("http://{address}/{channel}"),
                MpegTsRingConfig::new(3, 0).unwrap(),
            )
        };

        let mut pairs = Vec::new();
        for channel in ["one", "two", "three"] {
            let first = manager.open(source(channel)).await.unwrap();
            let second = manager.open(source(channel)).await.unwrap();
            assert_eq!(first.lease_id(), second.lease_id());
            pairs.push((first, second));
        }

        let snapshot = manager.provider_snapshot("provider").unwrap();
        assert_eq!(snapshot.active_sessions, 3);
        assert_eq!(snapshot.high_watermark, 3);
        assert_eq!(state.requests.load(Ordering::SeqCst), 3);
        assert!(
            state
                .requests_by_channel
                .lock()
                .unwrap()
                .values()
                .all(|requests| *requests == 1)
        );

        assert!(matches!(
            manager.open(source("four")).await,
            Err(SessionStartError::Provider(AcquireError::AtCapacity {
                capacity: 3,
                active_sessions: 3,
                ..
            }))
        ));

        timeout(Duration::from_secs(2), async {
            loop {
                if pairs
                    .iter()
                    .all(|(viewer, _)| viewer.ring_snapshot().first_sequence > 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("all three tiny rings must wrap");
        assert_eq!(state.requests.load(Ordering::SeqCst), 3);
        assert!(pairs.iter().all(|(viewer, _)| {
            let ring = viewer.ring_snapshot();
            ring.closed.is_none() && ring.retained_packets == 3
        }));

        let survivors: Vec<_> = pairs
            .into_iter()
            .map(|(first, second)| {
                drop(first);
                second
            })
            .collect();
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            3
        );
        assert_eq!(state.disconnects.load(Ordering::SeqCst), 0);

        drop(survivors);
        // Try the replacement immediately. The broker may reject it while old
        // HTTP tasks are cancelling, but must never grant a fourth slot.
        let replacement_source = source("four");
        let replacement = timeout(Duration::from_secs(2), async {
            loop {
                match manager.open(replacement_source.clone()).await {
                    Ok(viewer) => break viewer,
                    Err(SessionStartError::Provider(AcquireError::AtCapacity { .. })) => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("unexpected replacement error: {error}"),
                }
            }
        })
        .await
        .expect("cancelled sessions must release their provider slots within two seconds");
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .high_watermark,
            3,
            "fast replacement must never oversubscribe the provider cap"
        );
        assert_eq!(state.requests.load(Ordering::SeqCst), 4);

        drop(replacement);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.disconnects.load(Ordering::SeqCst) == 4
                    && manager
                        .provider_snapshot("provider")
                        .unwrap()
                        .active_sessions
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all upstreams must close and release within two seconds");

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn coordinated_failover_is_single_flight_generation_safe_and_sibling_is_unaffected() {
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct RecoveryState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            end_primary: Arc<Notify>,
        }

        async fn upstream(Path(source): Path<String>, State(state): State<RecoveryState>) -> Body {
            *state
                .requests
                .lock()
                .unwrap()
                .entry(source.clone())
                .or_default() += 1;
            let base = match source.as_str() {
                "primary" => 1,
                "alternate" => 100,
                "sibling" => 1_000,
                _ => unreachable!(),
            };
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(base));
                if source == "primary" {
                    state.end_primary.notified().await;
                    return;
                }
                let mut next = base + 2;
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, Infallible>(labelled_generation(next));
                    next = next.saturating_add(2);
                }
            };
            Body::from_stream(stream)
        }

        async fn next_media(stream: &mut ViewerByteStream) -> Bytes {
            loop {
                let bytes = timeout(Duration::from_secs(2), stream.next())
                    .await
                    .expect("viewer must remain live during coordinated recovery")
                    .expect("viewer stream must remain open")
                    .expect("recovery must not surface an error");
                if !bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet_pid(packet) == 0x1fff)
                {
                    return bytes;
                }
            }
        }

        let state = RecoveryState {
            requests: Arc::new(Mutex::new(HashMap::new())),
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/{source}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 2));
        manager.configure_provider(ProviderSpec::new("alternate-provider", 1));
        let recovery = RecoveryPolicy::new(
            Duration::from_millis(500),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut flaky = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "flaky", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        flaky.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-provider",
            format!("http://{address}/alternate?token=alternate-secret"),
        ));
        flaky.set_recovery_policy(recovery);
        let sibling = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "sibling", 1),
            format!("http://{address}/sibling"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );

        let flaky_first = manager.open(flaky.clone()).await.unwrap();
        let flaky_second = manager.open(flaky).await.unwrap();
        let sibling = manager.open(sibling).await.unwrap();
        assert_eq!(flaky_first.lease_id(), flaky_second.lease_id());
        let mut flaky_first = flaky_first.into_byte_stream();
        let mut flaky_second = flaky_second.into_byte_stream();
        let mut sibling = sibling.into_byte_stream();

        let initial_first = next_media(&mut flaky_first).await;
        let initial_second = next_media(&mut flaky_second).await;
        let sibling_initial = next_media(&mut sibling).await;
        assert!(
            media_labels(&initial_first)
                .iter()
                .all(|label| *label < 100)
        );
        assert_eq!(initial_first, initial_second);
        assert!(
            media_labels(&sibling_initial)
                .iter()
                .all(|label| *label >= 1_000)
        );

        state.end_primary.notify_waiters();
        let resumed_first = next_media(&mut flaky_first).await;
        let resumed_second = next_media(&mut flaky_second).await;
        assert_eq!(resumed_first, resumed_second);
        assert_eq!(packet_pid(&resumed_first[..MPEG_TS_PACKET_SIZE]), 0);
        assert!(
            media_labels(&resumed_first)
                .iter()
                .all(|label| *label >= 100),
            "one output chunk must never join old and new generations"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshots = manager.list_snapshots();
                let flaky = snapshots
                    .iter()
                    .find(|snapshot| snapshot.key.source_id.as_ref() == "flaky");
                if flaky.is_some_and(|snapshot| {
                    snapshot.state == SessionState::Streaming && snapshot.upstream_generation == 1
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        let snapshots = manager.list_snapshots();
        let flaky_snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.key.source_id.as_ref() == "flaky")
            .unwrap();
        let sibling_snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.key.source_id.as_ref() == "sibling")
            .unwrap();
        assert_eq!(flaky_snapshot.viewer_count, 2);
        assert_eq!(flaky_snapshot.reconnect_attempts, 1);
        assert_eq!(flaky_snapshot.failover_attempts, 1);
        assert_eq!(flaky_snapshot.failure_count, 1);
        assert_eq!(sibling_snapshot.state, SessionState::Streaming);
        assert_eq!(sibling_snapshot.upstream_generation, 0);
        assert_eq!(sibling_snapshot.reconnect_attempts, 0);
        let diagnostics = format!("{snapshots:?}");
        assert!(!diagnostics.contains("primary-secret"));
        assert!(!diagnostics.contains("alternate-secret"));

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.get("primary"), Some(&1));
        assert_eq!(requests.get("alternate"), Some(&1));
        assert_eq!(requests.get("sibling"), Some(&1));
        drop(requests);
        let pool = manager.provider_snapshot("provider").unwrap();
        assert_eq!(pool.active_sessions, 1);
        assert_eq!(pool.high_watermark, 2);
        let alternate_pool = manager.provider_snapshot("alternate-provider").unwrap();
        assert_eq!(alternate_pool.active_sessions, 1);
        assert_eq!(alternate_pool.high_watermark, 1);

        drop(flaky_first);
        drop(flaky_second);
        drop(sibling);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn recovery_expiry_emits_null_keepalive_then_closes_with_typed_error() {
        use std::{convert::Infallible, sync::Arc};

        use axum::{
            Router,
            body::Body,
            extract::State,
            http::StatusCode,
            response::{IntoResponse, Response},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct ExpiryState {
            requests: Arc<AtomicUsize>,
            end_initial: Arc<Notify>,
        }

        async fn upstream(State(state): State<ExpiryState>) -> Response {
            let request = state.requests.fetch_add(1, Ordering::SeqCst);
            if request > 0 {
                return (StatusCode::SERVICE_UNAVAILABLE, Body::empty()).into_response();
            }
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(1));
                state.end_initial.notified().await;
            };
            Body::from_stream(stream).into_response()
        }

        let state = ExpiryState {
            requests: Arc::new(AtomicUsize::new(0)),
            end_initial: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/live", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "expires", 1),
            format!("http://{address}/live?token=provider-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        source.set_recovery_policy(
            RecoveryPolicy::new(
                Duration::from_millis(120),
                Duration::from_millis(10),
                Duration::from_millis(10),
                Duration::from_millis(30),
            )
            .unwrap(),
        );
        let viewer = manager.open(source).await.unwrap();
        let mut stream = viewer.into_byte_stream();
        let initial = timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet_pid(&initial[..MPEG_TS_PACKET_SIZE]), 0);
        state.end_initial.notify_waiters();

        let mut saw_null_keepalive = false;
        let attempts = timeout(Duration::from_secs(2), async {
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        assert_eq!(bytes.len() % MPEG_TS_PACKET_SIZE, 0);
                        assert!(
                            bytes
                                .chunks_exact(MPEG_TS_PACKET_SIZE)
                                .all(|packet| packet_pid(packet) == 0x1fff)
                        );
                        saw_null_keepalive = true;
                    }
                    Some(Err(ViewerStreamError::RecoveryExpired { attempts })) => break attempts,
                    event => panic!("unexpected recovery-expiry event: {event:?}"),
                }
            }
        })
        .await
        .expect("bounded recovery must expire");
        assert!(saw_null_keepalive);
        assert!(attempts > 0);

        let snapshot = manager.list_snapshots().pop().unwrap();
        assert_eq!(snapshot.state, SessionState::Failed);
        assert_eq!(
            snapshot.last_failure,
            Some(SessionFailureKind::RecoveryExpired)
        );
        assert_eq!(snapshot.reconnect_attempts, attempts);
        assert!(snapshot.failure_count >= 2);
        assert!(!format!("{snapshot:?}").contains("provider-secret"));
        assert!(state.requests.load(Ordering::SeqCst) > 1);

        drop(stream);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn six_viewers_share_one_single_flight_reconnect_and_all_resume() {
        const VIEWER_COUNT: usize = 6;
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct RecoveryState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            end_primary: Arc<Notify>,
        }

        async fn upstream(Path(source): Path<String>, State(state): State<RecoveryState>) -> Body {
            *state
                .requests
                .lock()
                .unwrap()
                .entry(source.clone())
                .or_default() += 1;
            let base = match source.as_str() {
                "primary" => 1,
                "alternate" => 100,
                _ => unreachable!(),
            };
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(base));
                if source == "primary" {
                    state.end_primary.notified().await;
                    return;
                }
                let mut next = base + 2;
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, Infallible>(labelled_generation(next));
                    next = next.saturating_add(2);
                }
            };
            Body::from_stream(stream)
        }

        async fn next_media(stream: &mut ViewerByteStream) -> Bytes {
            loop {
                let bytes = timeout(Duration::from_secs(2), stream.next())
                    .await
                    .expect("viewer must remain live during coordinated recovery")
                    .expect("viewer stream must remain open")
                    .expect("recovery must not surface an error");
                if !bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet_pid(packet) == 0x1fff)
                {
                    return bytes;
                }
            }
        }

        let state = RecoveryState {
            requests: Arc::new(Mutex::new(HashMap::new())),
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/{source}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 2));
        manager.configure_provider(ProviderSpec::new("alternate-provider", 1));
        let recovery = RecoveryPolicy::new(
            Duration::from_millis(500),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut flaky = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "flaky-six", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        flaky.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-provider",
            format!("http://{address}/alternate?token=alternate-secret"),
        ));
        flaky.set_recovery_policy(recovery);

        let anchor = manager.open(flaky.clone()).await.unwrap();
        let anchor_lease = anchor.lease_id();
        let mut viewers: Vec<ViewerByteStream> = Vec::with_capacity(VIEWER_COUNT);
        viewers.push(anchor.into_byte_stream());
        for _ in 1..VIEWER_COUNT {
            let extra = manager.open(flaky.clone()).await.unwrap();
            assert_eq!(extra.lease_id(), anchor_lease);
            viewers.push(extra.into_byte_stream());
        }

        for viewer in &mut viewers {
            let initial = next_media(viewer).await;
            assert!(
                media_labels(&initial).iter().all(|label| *label < 100),
                "all viewers must start on the primary generation"
            );
        }
        assert_eq!(
            manager
                .list_snapshots()
                .into_iter()
                .next()
                .unwrap()
                .viewer_count,
            VIEWER_COUNT
        );

        state.end_primary.notify_waiters();
        for viewer in &mut viewers {
            let resumed = next_media(viewer).await;
            assert_eq!(packet_pid(&resumed[..MPEG_TS_PACKET_SIZE]), 0);
            assert!(
                media_labels(&resumed).iter().all(|label| *label >= 100),
                "all viewers must resume on the alternate generation"
            );
        }

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = manager
                    .list_snapshots()
                    .into_iter()
                    .next()
                    .expect("the shared session must remain live");
                if snapshot.state == SessionState::Streaming && snapshot.upstream_generation == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the single pump must recover for all six viewers");

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .next()
            .expect("the shared session must remain live");
        assert_eq!(snapshot.viewer_count, VIEWER_COUNT);
        assert_eq!(
            snapshot.reconnect_attempts, 1,
            "one pump serves all viewers as a single flight"
        );
        assert_eq!(snapshot.failover_attempts, 1);
        assert_eq!(snapshot.failure_count, 1);

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.get("primary"), Some(&1));
        assert_eq!(requests.get("alternate"), Some(&1));
        drop(requests);

        drop(viewers);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn same_pool_failover_retains_one_lease_and_recovers() {
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct RecoveryState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            end_primary: Arc<Notify>,
        }

        async fn upstream(Path(source): Path<String>, State(state): State<RecoveryState>) -> Body {
            *state
                .requests
                .lock()
                .unwrap()
                .entry(source.clone())
                .or_default() += 1;
            let base = match source.as_str() {
                "primary" => 1,
                "alternate" => 100,
                _ => unreachable!(),
            };
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(base));
                if source == "primary" {
                    state.end_primary.notified().await;
                    return;
                }
                let mut next = base + 2;
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, Infallible>(labelled_generation(next));
                    next = next.saturating_add(2);
                }
            };
            Body::from_stream(stream)
        }

        async fn next_media(stream: &mut ViewerByteStream) -> Bytes {
            loop {
                let bytes = timeout(Duration::from_secs(2), stream.next())
                    .await
                    .expect("viewer must remain live during same-pool recovery")
                    .expect("viewer stream must remain open")
                    .expect("same-pool recovery must not surface an error");
                if !bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet_pid(packet) == 0x1fff)
                {
                    return bytes;
                }
            }
        }

        let state = RecoveryState {
            requests: Arc::new(Mutex::new(HashMap::new())),
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/{source}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("shared-pool", 2));
        let recovery = RecoveryPolicy::new(
            Duration::from_millis(500),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("shared-pool", "same-pool-flaky", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        // The alternate endpoint omits an explicit pool, so it inherits the
        // source key pool. Both endpoints share one provider lease.
        source.add_alternate(HttpTsEndpoint::new(format!(
            "http://{address}/alternate?token=alternate-secret"
        )));
        source.set_recovery_policy(recovery);

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(
            manager
                .provider_snapshot("shared-pool")
                .unwrap()
                .active_sessions,
            1
        );

        let mut first = first.into_byte_stream();
        let mut second = second.into_byte_stream();
        let initial = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(media_labels(&initial).iter().all(|label| *label < 100));

        state.end_primary.notify_waiters();
        let resumed = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(
            media_labels(&resumed).iter().all(|label| *label >= 100),
            "same-pool failover must resume from the alternate endpoint"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = manager
                    .list_snapshots()
                    .into_iter()
                    .next()
                    .expect("the shared session must remain live");
                if snapshot.state == SessionState::Streaming && snapshot.upstream_generation == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("same-pool failover must recover");

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .next()
            .expect("the shared session must remain live");
        assert_eq!(snapshot.failover_attempts, 1);
        assert_eq!(snapshot.reconnect_attempts, 1);
        let pool = manager.provider_snapshot("shared-pool").unwrap();
        assert_eq!(
            pool.active_sessions, 1,
            "same-pool failover must not acquire a second lease"
        );
        assert_eq!(pool.high_watermark, 1);

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.get("primary"), Some(&1));
        assert_eq!(requests.get("alternate"), Some(&1));
        drop(requests);

        drop(first);
        drop(second);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn provider_capacity_during_recovery_retries_and_recovers_via_primary() {
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct RecoveryState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            end_primary: Arc<Notify>,
        }

        async fn upstream(Path(source): Path<String>, State(state): State<RecoveryState>) -> Body {
            let request_number = {
                let mut requests = state.requests.lock().unwrap();
                let count = requests.entry(source.clone()).or_default();
                *count += 1;
                *count
            };
            let base = match source.as_str() {
                "primary" if request_number == 1 => 1,
                "primary" => 5,
                "alternate" => 100,
                "occupier" => 200,
                _ => unreachable!(),
            };
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(base));
                if source == "primary" && request_number == 1 {
                    state.end_primary.notified().await;
                    return;
                }
                let mut next = base + 2;
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, Infallible>(labelled_generation(next));
                    next = next.saturating_add(2);
                }
            };
            Body::from_stream(stream)
        }

        async fn next_media(stream: &mut ViewerByteStream) -> Bytes {
            loop {
                let bytes = timeout(Duration::from_secs(2), stream.next())
                    .await
                    .expect("viewer must remain live during capacity-constrained recovery")
                    .expect("viewer stream must remain open")
                    .expect("recovery must not surface an error");
                if !bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet_pid(packet) == 0x1fff)
                {
                    return bytes;
                }
            }
        }

        let state = RecoveryState {
            requests: Arc::new(Mutex::new(HashMap::new())),
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/{source}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("primary-pool", 1));
        manager.configure_provider(ProviderSpec::new("alternate-pool", 1));

        // Occupy the alternate pool so recovery cannot acquire a second lease.
        let occupier = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("alternate-pool", "occupier", 1),
            format!("http://{address}/occupier"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        let occupier_viewer = manager.open(occupier).await.unwrap();
        assert_eq!(
            manager
                .provider_snapshot("alternate-pool")
                .unwrap()
                .active_sessions,
            1
        );

        let recovery = RecoveryPolicy::new(
            Duration::from_millis(800),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut flaky = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("primary-pool", "capacity-flaky", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        flaky.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-pool",
            format!("http://{address}/alternate?token=alternate-secret"),
        ));
        flaky.set_recovery_policy(recovery);

        let first = manager.open(flaky.clone()).await.unwrap();
        let second = manager.open(flaky).await.unwrap();
        let mut first = first.into_byte_stream();
        let mut second = second.into_byte_stream();
        let initial = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(media_labels(&initial).iter().all(|label| *label < 5));

        state.end_primary.notify_waiters();
        let resumed = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(
            media_labels(&resumed)
                .iter()
                .all(|label| *label >= 5 && *label < 100),
            "recovery must resume from the primary after the alternate pool rejected capacity"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = manager
                    .list_snapshots()
                    .into_iter()
                    .find(|snapshot| snapshot.key.source_id.as_ref() == "capacity-flaky")
                    .expect("the flaky session must remain live");
                if snapshot.state == SessionState::Streaming {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("recovery must succeed after capacity rejection");

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.key.source_id.as_ref() == "capacity-flaky")
            .expect("the flaky session must remain live");
        assert!(
            snapshot.reconnect_attempts >= 2,
            "recovery must retry after the alternate pool rejects capacity"
        );
        let alternate_pool = manager.provider_snapshot("alternate-pool").unwrap();
        assert_eq!(alternate_pool.active_sessions, 1);
        assert_eq!(alternate_pool.high_watermark, 1);

        let requests = state.requests.lock().unwrap();
        assert_eq!(
            requests.get("primary"),
            Some(&2),
            "the primary must serve the initial and recovery requests"
        );
        assert_eq!(
            requests.get("alternate"),
            None,
            "capacity rejection must prevent any alternate HTTP request"
        );
        drop(requests);

        drop(first);
        drop(second);
        drop(occupier_viewer);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn recovery_response_timeout_expires_with_typed_http_failures() {
        use std::convert::Infallible;

        use axum::{
            Router,
            body::Body,
            extract::State,
            response::{IntoResponse, Response},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct TimeoutState {
            requests: Arc<AtomicUsize>,
            end_initial: Arc<Notify>,
        }

        async fn upstream(State(state): State<TimeoutState>) -> Response {
            let request = state.requests.fetch_add(1, Ordering::SeqCst);
            if request == 0 {
                let stream = async_stream::stream! {
                    yield Ok::<_, Infallible>(labelled_generation(1));
                    state.end_initial.notified().await;
                };
                return Body::from_stream(stream).into_response();
            }
            // Recovery requests never respond; they exceed the startup timeout.
            std::future::pending::<Response>().await
        }

        let state = TimeoutState {
            requests: Arc::new(AtomicUsize::new(0)),
            end_initial: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/live", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "hangs", 1),
            format!("http://{address}/live?token=provider-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        source.set_startup_timeout(Duration::from_millis(40));
        source.set_recovery_policy(
            RecoveryPolicy::new(
                Duration::from_millis(300),
                Duration::from_millis(5),
                Duration::from_millis(20),
                Duration::from_millis(30),
            )
            .unwrap(),
        );
        let viewer = manager.open(source).await.unwrap();
        let mut stream = viewer.into_byte_stream();
        let initial = timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet_pid(&initial[..MPEG_TS_PACKET_SIZE]), 0);
        state.end_initial.notify_waiters();

        let attempts = timeout(Duration::from_secs(2), async {
            loop {
                match stream.next().await {
                    Some(Ok(_)) => {}
                    Some(Err(ViewerStreamError::RecoveryExpired { attempts })) => break attempts,
                    event => panic!("unexpected recovery-timeout event: {event:?}"),
                }
            }
        })
        .await
        .expect("bounded recovery must expire when responses hang");

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .next()
            .expect("the session must remain observable until expiry");
        assert_eq!(snapshot.state, SessionState::Failed);
        assert_eq!(
            snapshot.last_failure,
            Some(SessionFailureKind::RecoveryExpired)
        );
        assert_eq!(snapshot.reconnect_attempts, attempts);
        assert!(
            snapshot.failure_count >= 2,
            "the initial failure and at least one timeout must be recorded"
        );
        assert!(
            state.requests.load(Ordering::SeqCst) > 1,
            "recovery must attempt more than one request before expiry"
        );
        assert!(!format!("{snapshot:?}").contains("provider-secret"));

        drop(stream);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn keepalive_null_packets_respect_the_configured_interval() {
        use std::convert::Infallible;

        use axum::{
            Router,
            body::Body,
            extract::State,
            response::{IntoResponse, Response},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct KeepaliveState {
            requests: Arc<AtomicUsize>,
            end_initial: Arc<Notify>,
        }

        async fn upstream(State(state): State<KeepaliveState>) -> Response {
            let request = state.requests.fetch_add(1, Ordering::SeqCst);
            if request == 0 {
                let stream = async_stream::stream! {
                    yield Ok::<_, Infallible>(labelled_generation(1));
                    state.end_initial.notified().await;
                };
                return Body::from_stream(stream).into_response();
            }
            std::future::pending::<Response>().await
        }

        let state = KeepaliveState {
            requests: Arc::new(AtomicUsize::new(0)),
            end_initial: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/live", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "keepalive-timing", 1),
            format!("http://{address}/live?token=provider-secret"),
            MpegTsRingConfig::new(64, 64).unwrap(),
        );
        source.set_startup_timeout(Duration::from_millis(30));
        let keepalive_interval = Duration::from_millis(50);
        source.set_recovery_policy(
            RecoveryPolicy::new(
                Duration::from_millis(500),
                Duration::from_millis(10),
                keepalive_interval,
                Duration::from_millis(30),
            )
            .unwrap(),
        );
        let viewer = manager.open(source).await.unwrap();
        let mut stream = viewer.into_byte_stream();
        let initial = timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet_pid(&initial[..MPEG_TS_PACKET_SIZE]), 0);
        state.end_initial.notify_waiters();

        let mut null_packets = 0_u32;
        let attempts = timeout(Duration::from_secs(2), async {
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        assert_eq!(bytes.len() % MPEG_TS_PACKET_SIZE, 0);
                        if bytes
                            .chunks_exact(MPEG_TS_PACKET_SIZE)
                            .all(|packet| packet_pid(packet) == 0x1fff)
                        {
                            null_packets = null_packets.saturating_add(
                                u32::try_from(bytes.len() / MPEG_TS_PACKET_SIZE)
                                    .unwrap_or(u32::MAX),
                            );
                        }
                    }
                    Some(Err(ViewerStreamError::RecoveryExpired { attempts })) => break attempts,
                    event => panic!("unexpected keepalive-timing event: {event:?}"),
                }
            }
        })
        .await
        .expect("bounded recovery must expire");

        // The recovery window is 500 ms and the keepalive interval is 50 ms.
        // Assert a bounded range so the test stays deterministic across CI
        // schedulers while it still verifies interval-paced emission.
        assert!(
            null_packets >= 3,
            "keepalive must emit null packets at the configured interval; saw {null_packets}"
        );
        assert!(
            null_packets <= 20,
            "keepalive must not emit null packets faster than the configured interval; saw {null_packets}"
        );
        assert!(attempts > 0);

        drop(stream);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn multi_alternate_sequencing_cycles_to_the_second_alternate() {
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            http::StatusCode,
            response::{IntoResponse, Response},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct SequencingState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            end_primary: Arc<Notify>,
        }

        async fn upstream(
            Path(source): Path<String>,
            State(state): State<SequencingState>,
        ) -> Response {
            *state
                .requests
                .lock()
                .unwrap()
                .entry(source.clone())
                .or_default() += 1;
            match source.as_str() {
                "primary" => {
                    let stream = async_stream::stream! {
                        yield Ok::<_, Infallible>(labelled_generation(1));
                        state.end_primary.notified().await;
                    };
                    Body::from_stream(stream).into_response()
                }
                "alternate-one" => (StatusCode::SERVICE_UNAVAILABLE, Body::empty()).into_response(),
                "alternate-two" => {
                    let stream = async_stream::stream! {
                        let mut next = 200_u32;
                        loop {
                            yield Ok::<_, Infallible>(labelled_generation(next));
                            next = next.saturating_add(2);
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    };
                    Body::from_stream(stream).into_response()
                }
                _ => unreachable!(),
            }
        }

        async fn next_media(stream: &mut ViewerByteStream) -> Bytes {
            loop {
                let bytes = timeout(Duration::from_secs(2), stream.next())
                    .await
                    .expect("viewer must remain live during multi-alternate recovery")
                    .expect("viewer stream must remain open")
                    .expect("multi-alternate recovery must not surface an error");
                if !bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet_pid(packet) == 0x1fff)
                {
                    return bytes;
                }
            }
        }

        let state = SequencingState {
            requests: Arc::new(Mutex::new(HashMap::new())),
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/{source}", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("primary-pool", 1));
        manager.configure_provider(ProviderSpec::new("alternate-pool", 2));
        let recovery = RecoveryPolicy::new(
            Duration::from_millis(800),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("primary-pool", "multi-alternate", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        source.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-pool",
            format!("http://{address}/alternate-one?token=alt-one-secret"),
        ));
        source.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-pool",
            format!("http://{address}/alternate-two?token=alt-two-secret"),
        ));
        source.set_recovery_policy(recovery);

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        let mut first = first.into_byte_stream();
        let mut second = second.into_byte_stream();
        let initial = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(media_labels(&initial).iter().all(|label| *label < 100));

        state.end_primary.notify_waiters();
        let resumed = next_media(&mut first).await;
        let _ = next_media(&mut second).await;
        assert!(
            media_labels(&resumed).iter().all(|label| *label >= 200),
            "recovery must sequence past the failed first alternate to the second"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = manager
                    .list_snapshots()
                    .into_iter()
                    .find(|snapshot| snapshot.key.source_id.as_ref() == "multi-alternate")
                    .expect("the session must remain live");
                if snapshot.state == SessionState::Streaming {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("multi-alternate recovery must complete");

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.key.source_id.as_ref() == "multi-alternate")
            .expect("the session must remain live");
        assert_eq!(
            snapshot.reconnect_attempts, 2,
            "recovery must try the first alternate then the second"
        );
        assert_eq!(snapshot.failover_attempts, 2);

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.get("primary"), Some(&1));
        assert_eq!(requests.get("alternate-one"), Some(&1));
        assert_eq!(requests.get("alternate-two"), Some(&1));
        drop(requests);

        drop(first);
        drop(second);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn resumed_generation_boundary_includes_pat_then_pmt_before_media() {
        use std::{convert::Infallible, sync::Arc};

        use axum::{
            Router,
            body::Body,
            extract::State,
            response::{IntoResponse, Response},
            routing::get,
        };
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        #[derive(Clone)]
        struct BoundaryState {
            end_primary: Arc<Notify>,
        }

        async fn upstream(State(state): State<BoundaryState>) -> Response {
            let stream = async_stream::stream! {
                yield Ok::<_, Infallible>(labelled_generation(1));
                state.end_primary.notified().await;
            };
            Body::from_stream(stream).into_response()
        }

        let state = BoundaryState {
            end_primary: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/primary", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        manager.configure_provider(ProviderSpec::new("alternate-provider", 1));
        let recovery = RecoveryPolicy::new(
            Duration::from_millis(500),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(100),
        )
        .unwrap();
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "boundary", 1),
            format!("http://{address}/primary?token=primary-secret"),
            MpegTsRingConfig::new(32, 32).unwrap(),
        );
        source.add_alternate(HttpTsEndpoint::for_provider(
            "alternate-provider",
            format!("http://{address}/primary?token=alternate-secret"),
        ));
        source.set_recovery_policy(recovery);

        let viewer = manager.open(source).await.unwrap();
        let mut stream = viewer.into_byte_stream();

        // Drain the initial generation until the first non-null media chunk.
        let mut saw_initial = false;
        let initial = timeout(Duration::from_secs(2), async {
            loop {
                let bytes = stream
                    .next()
                    .await
                    .expect("initial stream must remain open")
                    .expect("initial stream must not error");
                if bytes
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .any(|packet| packet_pid(packet) != 0x1fff)
                {
                    saw_initial = true;
                    return bytes;
                }
            }
        })
        .await
        .expect("initial media must arrive");
        assert!(saw_initial);
        // The initial generation must also start with PAT then PMT.
        assert_eq!(packet_pid(&initial[..MPEG_TS_PACKET_SIZE]), 0);
        assert_eq!(
            packet_pid(&initial[MPEG_TS_PACKET_SIZE..2 * MPEG_TS_PACKET_SIZE]),
            TEST_PMT_PID
        );

        state.end_primary.notify_waiters();

        // Collect the resumed generation. Skip null keepalive packets and
        // gather the first non-null chunk so the boundary is observable.
        let resumed = timeout(Duration::from_secs(2), async {
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        if bytes
                            .chunks_exact(MPEG_TS_PACKET_SIZE)
                            .any(|packet| packet_pid(packet) != 0x1fff)
                        {
                            return bytes;
                        }
                    }
                    Some(Err(error)) => panic!("recovery must not error: {error}"),
                    None => panic!("recovery must not close the viewer"),
                }
            }
        })
        .await
        .expect("resumed media must arrive after failover");

        let pids: Vec<u16> = resumed
            .chunks_exact(MPEG_TS_PACKET_SIZE)
            .map(packet_pid)
            .collect();
        assert_eq!(
            pids[0], 0,
            "the resumed generation must start with a PAT packet"
        );
        assert_eq!(
            pids[1], TEST_PMT_PID,
            "the resumed generation must include a PMT packet before any media"
        );
        assert!(
            pids.contains(&TEST_VIDEO_PID),
            "the resumed generation must include media packets after PAT and PMT"
        );
        // No media packet may appear before both PAT and PMT.
        let media_index = pids
            .iter()
            .position(|pid| *pid == TEST_VIDEO_PID)
            .expect("media must be present");
        assert!(
            media_index >= 2,
            "PAT and PMT must precede the first media packet; saw media at index {media_index}"
        );

        let snapshot = manager
            .list_snapshots()
            .into_iter()
            .next()
            .expect("the session must remain live");
        assert_eq!(snapshot.upstream_generation, 1);
        assert_eq!(snapshot.reconnect_attempts, 1);

        drop(stream);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn native_http_ts_two_viewers_share_one_session_wrap_ring_without_restart_and_shutdown_within_two_seconds()
     {
        use std::convert::Infallible;

        use axum::{Router, body::Body, extract::State, http::header, routing::get};
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            disconnects: Arc<AtomicUsize>,
        }

        struct DisconnectGuard(Arc<AtomicUsize>);

        impl Drop for DisconnectGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn upstream(State(state): State<UpstreamState>) -> Body {
            state.requests.fetch_add(1, Ordering::SeqCst);
            let disconnects = Arc::clone(&state.disconnects);
            let stream = async_stream::stream! {
                let _guard = DisconnectGuard(disconnects);
                let mut id = 0_u32;
                loop {
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&packet(id)));
                    id = id.wrapping_add(1);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            Body::from_stream(stream)
        }

        let state = UpstreamState {
            requests: Arc::new(AtomicUsize::new(0)),
            disconnects: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/live.ts", get(upstream))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "native-wrap", 1),
            format!("http://{address}/live.ts?username=provider-user&password=provider-password"),
            MpegTsRingConfig::new(3, 0).unwrap(),
        );
        source.headers_mut().insert(
            header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer provider-secret"),
        );

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(first.input_adapter(), SessionInputAdapter::NativeTs);
        assert_eq!(second.input_adapter(), SessionInputAdapter::NativeTs);
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1
        );

        // Wait for the bounded ring to wrap while the single HTTP request keeps
        // streaming. A wrapped ring with one request proves no restart.
        timeout(Duration::from_secs(5), async {
            loop {
                let ring = first.ring_snapshot();
                if ring.first_sequence > 0
                    && ring.retained_packets == 3
                    && ring.closed.is_none()
                    && first.session_snapshot().state == SessionState::Streaming
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the native ring must wrap without restarting the upstream");
        assert_eq!(
            state.requests.load(Ordering::SeqCst),
            1,
            "ring wrap must not restart the native HTTP request"
        );
        let diagnostics = format!("{:?}", manager.list_adapter_diagnostics());
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(
                !diagnostics.contains(secret),
                "native diagnostics must not disclose {secret}"
            );
        }

        drop(first);
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1,
            "one remaining viewer must keep the shared session alive"
        );
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.disconnects.load(Ordering::SeqCst) == 1
                    && manager
                        .provider_snapshot("provider")
                        .unwrap()
                        .active_sessions
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the final native viewer must shut down within two seconds");
        assert_eq!(state.disconnects.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_lines)]
    async fn fixed_adapter_ring_wrap_keeps_one_process_and_one_request(
        policy: InputAdapterPolicy,
        expected_adapter: SessionInputAdapter,
        upstream_is_live: bool,
    ) {
        use std::convert::Infallible;

        use axum::{
            Router,
            body::Body,
            extract::State,
            http::{HeaderMap, HeaderValue, header},
            response::Response,
            routing::get,
        };
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            disconnects: Arc<AtomicUsize>,
            fixture: Arc<Vec<u8>>,
            live: bool,
        }

        struct DisconnectGuard(Arc<AtomicUsize>);

        impl Drop for DisconnectGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn upstream(State(state): State<UpstreamState>, headers: HeaderMap) -> Response {
            assert_eq!(
                headers.get(header::AUTHORIZATION),
                Some(&HeaderValue::from_static("Bearer provider-secret"))
            );
            state.requests.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                let _guard = DisconnectGuard(Arc::clone(&state.disconnects));
                loop {
                    yield Ok::<_, Infallible>(Bytes::copy_from_slice(&state.fixture));
                    if !state.live {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            let mut response = Response::new(Body::from_stream(stream));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
            response
        }

        let state = UpstreamState {
            requests: Arc::new(AtomicUsize::new(0)),
            disconnects: Arc::new(AtomicUsize::new(0)),
            fixture: Arc::new(fixed_adapter_fixture()),
            live: upstream_is_live,
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/live.ts", get(upstream))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "wrap-channel", 1),
            format!("http://{address}/live.ts?username=provider-user&password=provider-password"),
            MpegTsRingConfig::new(3, 0).unwrap(),
        );
        source.set_adapter_policy(policy);
        source.headers_mut().insert(
            header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer provider-secret"),
        );

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(first.input_adapter(), expected_adapter);
        assert_eq!(second.input_adapter(), expected_adapter);
        let diagnostics = manager.list_adapter_diagnostics();
        assert_eq!(diagnostics.len(), 1, "two viewers must share one process");
        assert!(diagnostics[0].process_id.is_some());
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1,
            "two viewers must share one provider slot"
        );

        // Wait for the bounded ring to wrap while one process keeps streaming.
        timeout(Duration::from_secs(5), async {
            loop {
                let ring = first.ring_snapshot();
                let wrapped = ring.next_sequence > 3 && ring.retained_packets == 3;
                let still_open = if upstream_is_live {
                    ring.closed.is_none()
                        && first.session_snapshot().state == SessionState::Streaming
                } else {
                    true
                };
                if wrapped && still_open {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the fixed adapter must wrap its bounded ring without exit");
        assert_eq!(
            state.requests.load(Ordering::SeqCst),
            1,
            "ring wrap must not restart the fixed adapter upstream request"
        );
        let diagnostic_debug = format!("{:?}", manager.list_adapter_diagnostics());
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(
                !diagnostic_debug.contains(secret),
                "fixed adapter diagnostics must not disclose {secret}"
            );
        }

        drop(first);
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.disconnects.load(Ordering::SeqCst) == 1
                    && manager
                        .provider_snapshot("provider")
                        .unwrap()
                        .active_sessions
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the final fixed adapter viewer must shut down within two seconds");

        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ffmpeg_ring_wrap_does_not_restart_the_upstream() {
        fixed_adapter_ring_wrap_keeps_one_process_and_one_request(
            InputAdapterPolicy::Ffmpeg,
            SessionInputAdapter::Ffmpeg,
            true,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn vlc_ring_wrap_does_not_restart_the_upstream() {
        fixed_adapter_ring_wrap_keeps_one_process_and_one_request(
            InputAdapterPolicy::Vlc,
            SessionInputAdapter::Vlc,
            false,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn hls_ffmpeg_ring_wrap_does_not_restart_manifest_and_shares_one_process_slot() {
        use std::{
            collections::HashMap,
            sync::{Arc, Mutex},
        };

        use axum::{
            Router,
            body::Body,
            extract::{Path, State},
            http::{HeaderMap, HeaderValue, header},
            response::Response,
            routing::get,
        };
        use tokio::{net::TcpListener, time::timeout};

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<Mutex<HashMap<String, usize>>>,
            fixture: Arc<Vec<u8>>,
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
            let body = match path.as_str() {
                "master.m3u8" => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=1\nnested/child.m3u8\n".as_bytes().to_vec(),
                "nested/child.m3u8" => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,Segment\nsegment.ts\n#EXT-X-ENDLIST\n".as_bytes().to_vec(),
                "nested/segment.ts" => state.fixture.as_ref().clone(),
                _ => Vec::new(),
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
            requests: Arc::new(Mutex::new(HashMap::new())),
            fixture: Arc::new(fixed_adapter_fixture()),
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
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

        let manager = HttpTsSessionManager::new(Client::new());
        manager.configure_provider(ProviderSpec::new("provider", 1));
        let mut source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("provider", "hls-wrap", 1),
            format!(
                "http://{address}/master.m3u8?username=provider-user&password=provider-password"
            ),
            MpegTsRingConfig::new(3, 0).unwrap(),
        );
        source.set_adapter_policy(InputAdapterPolicy::Ffmpeg);
        source
            .set_process_input_format(BrokeredInputFormat::Hls(crate::HlsBrokerConfig::default()));
        source.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-secret"),
        );

        let first = manager.open(source.clone()).await.unwrap();
        let second = manager.open(source).await.unwrap();
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(first.input_adapter(), SessionInputAdapter::Ffmpeg);
        let diagnostics = manager.list_adapter_diagnostics();
        assert_eq!(
            diagnostics.len(),
            1,
            "two HLS viewers must share one process"
        );
        assert!(diagnostics[0].process_id.is_some());
        assert_eq!(
            manager
                .provider_snapshot("provider")
                .unwrap()
                .active_sessions,
            1,
            "two HLS viewers must share one provider slot"
        );

        // Wait for the bounded ring to wrap while the single FFmpeg process
        // reads the manifest and segment exactly once.
        timeout(Duration::from_secs(5), async {
            loop {
                let ring = first.ring_snapshot();
                if ring.next_sequence > 3 && ring.retained_packets == 3 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the HLS ring must wrap during one segment read");
        {
            let requests = state.requests.lock().unwrap();
            assert_eq!(
                requests.get("master.m3u8"),
                Some(&1),
                "ring wrap must not restart the HLS manifest fetch"
            );
            assert_eq!(requests.get("nested/child.m3u8"), Some(&1));
            assert_eq!(requests.get("nested/segment.ts"), Some(&1));
        }
        let diagnostic_debug = format!("{:?}", manager.list_adapter_diagnostics());
        for secret in ["provider-user", "provider-password", "provider-secret"] {
            assert!(
                !diagnostic_debug.contains(secret),
                "HLS diagnostics must not disclose {secret}"
            );
        }

        drop(first);
        drop(second);
        timeout(Duration::from_secs(2), async {
            loop {
                if manager
                    .provider_snapshot("provider")
                    .unwrap()
                    .active_sessions
                    == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the final HLS viewer must release the process slot within two seconds");

        server.abort();
    }
}
