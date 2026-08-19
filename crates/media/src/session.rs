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

use crate::{
    AcquireError, HttpTsSessionSnapshot, MPEG_TS_PACKET_SIZE, MpegTsRing, MpegTsRingConfig,
    PoolSnapshot, ProviderSlotBroker, RecoveryPolicy, RingCloseReason, RingRead, RingReadError,
    SessionFailureKind, SessionState, SharedSessionRegistry, SlotLease,
    psi::PatPmtTracker,
    recovery::{SessionDiagnostics, ViewerRegistration},
};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_VIEWER_BATCH_PACKETS: NonZeroUsize = NonZeroUsize::new(32).unwrap();
const MAX_PRIMING_PACKETS: usize = 16_384;

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
    url: Arc<str>,
    headers: HeaderMap,
}

impl fmt::Debug for HttpTsEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsEndpoint")
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
            url: url.into(),
            headers: HeaderMap::new(),
        }
    }

    /// Headers may contain credentials; they are never included in debug or
    /// session diagnostic output.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }
}

/// Request, buffering, and bounded recovery policy for an HTTP MPEG-TS source.
///
/// `generation` must change when the effective URL or headers change; active
/// viewers of an older generation are then isolated from the new session.
#[derive(Clone)]
pub struct HttpTsSourceSpec {
    key: HttpTsSessionKey,
    endpoints: Vec<HttpTsEndpoint>,
    ring: MpegTsRingConfig,
    startup_timeout: Duration,
    viewer_batch_packets: NonZeroUsize,
    recovery: RecoveryPolicy,
}

impl fmt::Debug for HttpTsSourceSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTsSourceSpec")
            .field("key", &self.key)
            .field("primary", &self.endpoints[0])
            .field("alternate_count", &self.endpoints.len().saturating_sub(1))
            .field("ring", &self.ring)
            .field("startup_timeout", &self.startup_timeout)
            .field("viewer_batch_packets", &self.viewer_batch_packets)
            .field("recovery", &self.recovery)
            .finish()
    }
}

impl HttpTsSourceSpec {
    pub fn new(key: HttpTsSessionKey, url: impl Into<Arc<str>>, ring: MpegTsRingConfig) -> Self {
        Self {
            key,
            endpoints: vec![HttpTsEndpoint::new(url)],
            ring,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            viewer_batch_packets: DEFAULT_VIEWER_BATCH_PACKETS,
            recovery: RecoveryPolicy::default(),
        }
    }

    pub fn key(&self) -> &HttpTsSessionKey {
        &self.key
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
            && self.endpoints == other.endpoints
            && self.ring == other.ring
            && self.startup_timeout == other.startup_timeout
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
    providers: DashMap<Arc<str>, ProviderSlotBroker>,
    sessions: SharedSessionRegistry<HttpTsSessionKey, HttpTsSession, SessionStartError>,
    session_index: DashMap<HttpTsSessionKey, Weak<HttpTsSession>>,
}

impl HttpTsSessionManager {
    pub fn new(client: Client) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                client,
                providers: DashMap::new(),
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

    /// Opens an independent viewer cursor on a single-flight shared session.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is unknown/full, response headers do
    /// not arrive before the startup timeout, or the HTTP request/status fails.
    pub async fn open(&self, source: HttpTsSourceSpec) -> Result<ViewerHandle, SessionStartError> {
        let broker = self
            .inner
            .providers
            .get(&source.key.provider_pool_id)
            .map(|broker| broker.clone())
            .ok_or_else(|| SessionStartError::UnknownProvider {
                pool_id: Arc::clone(&source.key.provider_pool_id),
            })?;
        let key = source.key.clone();
        let index_key = key.clone();
        let client = self.inner.client.clone();
        let session = self
            .inner
            .sessions
            .get_or_try_init(key, || async move {
                start_http_ts_session(client, broker, source).await
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
}

struct HttpTsSession {
    key: HttpTsSessionKey,
    ring: MpegTsRing,
    lease_id: u64,
    task: AbortHandle,
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
}

impl Drop for HttpTsSession {
    fn drop(&mut self) {
        // The task owns the lease behind its response body. Aborting it drops
        // the HTTP adapter first and the lease second, so a replacement cannot
        // briefly oversubscribe the provider while the socket is still live.
        self.diagnostics.set_state(SessionState::Stopping);
        self.ring.close(RingCloseReason::Shutdown);
        self.task.abort();
    }
}

type HttpBody = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>;

struct PumpResources {
    // Rust drops struct fields in declaration order. The body—and therefore
    // response/socket adapter—is gone before provider accounting is released.
    body: Option<HttpBody>,
    _lease: SlotLease,
}

async fn start_http_ts_session(
    client: Client,
    broker: ProviderSlotBroker,
    source: HttpTsSourceSpec,
) -> Result<HttpTsSession, SessionStartError> {
    let diagnostics = SessionDiagnostics::new(source.key.clone());
    diagnostics.set_state(SessionState::Reserving);
    let lease = broker.try_acquire(source.key.allocation_key())?;
    let lease_id = lease.lease_id();
    diagnostics.set_state(SessionState::Starting);
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
    let pump_config = HttpPumpConfig {
        client,
        endpoints: source.endpoints,
        diagnostics: Arc::clone(&diagnostics),
        recovery: source.recovery,
        startup_timeout: source.startup_timeout,
    };
    let task = tokio::spawn(pump_http_ts(response, ring.clone(), lease, pump_config));
    let abort_handle = task.abort_handle();
    drop(task);

    Ok(HttpTsSession {
        key: source.key,
        ring,
        lease_id,
        task: abort_handle,
        viewer_batch_packets: source.viewer_batch_packets,
        diagnostics,
    })
}

fn sanitize_reqwest_error(error: reqwest::Error) -> SessionStartError {
    SessionStartError::Http {
        message: error.without_url().to_string().into(),
    }
}

async fn pump_http_ts(
    response: Response,
    ring: MpegTsRing,
    lease: SlotLease,
    config: HttpPumpConfig,
) {
    let HttpPumpConfig {
        client,
        endpoints,
        diagnostics,
        recovery,
        startup_timeout,
    } = config;
    let mut resources = PumpResources {
        body: Some(Box::pin(response.bytes_stream())),
        _lease: lease,
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
                SessionFailureKind::Packetization
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
                )
                .await
            }
        }
        Err(failure) => failure,
    };

    loop {
        diagnostics.record_failure(active_failure);
        resources.body.take();
        let Some(recovered) = recover_session(
            &client,
            &endpoints,
            current_endpoint,
            &ring,
            &diagnostics,
            recovery,
            startup_timeout,
        )
        .await
        else {
            diagnostics.record_failure(SessionFailureKind::RecoveryExpired);
            diagnostics.set_state(SessionState::Failed);
            ring.close(RingCloseReason::RecoveryExpired {
                attempts: diagnostics.reconnect_attempts(),
            });
            return;
        };

        current_endpoint = recovered.endpoint_index;
        packetizer = recovered.packetizer;
        resources.body = Some(recovered.body);
        active_failure = stream_until_failure(
            resources.body.as_mut().expect("recovered body exists"),
            &ring,
            &diagnostics,
            &mut packetizer,
        )
        .await;
    }
}

struct HttpPumpConfig {
    client: Client,
    endpoints: Vec<HttpTsEndpoint>,
    diagnostics: Arc<SessionDiagnostics>,
    recovery: RecoveryPolicy,
    startup_timeout: Duration,
}

struct RecoveredStream {
    endpoint_index: usize,
    body: HttpBody,
    packetizer: MpegTsPacketizer,
}

async fn recover_session(
    client: &Client,
    endpoints: &[HttpTsEndpoint],
    previous_endpoint: usize,
    ring: &MpegTsRing,
    diagnostics: &Arc<SessionDiagnostics>,
    recovery: RecoveryPolicy,
    startup_timeout: Duration,
) -> Option<RecoveredStream> {
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
        let failover_offset = usize::from(endpoints.len() > 1);
        let endpoint_index = (previous_endpoint + failover_offset + attempt) % endpoints.len();
        let is_failover = endpoint_index != previous_endpoint;
        diagnostics.set_state(if is_failover {
            SessionState::FailingOver
        } else {
            SessionState::Recovering
        });
        diagnostics.record_reconnect(is_failover);

        let request_deadline = deadline.min(Instant::now() + startup_timeout);
        let response = tokio::time::timeout_at(
            request_deadline,
            client
                .get(endpoints[endpoint_index].url.as_ref())
                .headers(endpoints[endpoint_index].headers.clone())
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

        keepalive.stop().await;
        let generation = ring.start_new_generation().ok()?;
        diagnostics.set_generation(generation);
        if push_packets(ring, diagnostics, &primed).is_err() {
            diagnostics.record_failure(SessionFailureKind::Packetization);
            return None;
        }
        diagnostics.set_state(SessionState::Streaming);
        return Some(RecoveredStream {
            endpoint_index,
            body,
            packetizer,
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
            .map_err(|_| SessionFailureKind::Priming)?
            .ok_or(SessionFailureKind::UpstreamEnded)?
            .map_err(|_| SessionFailureKind::Http)?;
        let packets = packetizer
            .push(&chunk)
            .map_err(|_| SessionFailureKind::Packetization)?;
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
) -> SessionFailureKind {
    loop {
        match body.next().await {
            Some(Ok(chunk)) => match packetizer.push(&chunk) {
                Ok(packets) => {
                    if push_packets(ring, diagnostics, &packets).is_err() {
                        return SessionFailureKind::Packetization;
                    }
                }
                Err(_) => return SessionFailureKind::Packetization,
            },
            Some(Err(_)) => return SessionFailureKind::Http,
            None => {
                return if packetizer.pending_bytes() == 0 {
                    SessionFailureKind::UpstreamEnded
                } else {
                    SessionFailureKind::Packetization
                };
            }
        }
    }
}

fn push_packets(
    ring: &MpegTsRing,
    diagnostics: &SessionDiagnostics,
    packets: &[u8],
) -> Result<(), ()> {
    if packets.is_empty() {
        return Ok(());
    }
    let outcome = ring.push(packets).map_err(|_| ())?;
    diagnostics.record_write(outcome.packets_overwritten);
    Ok(())
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

    use crate::psi::test_support::{TEST_VIDEO_PID, pat_packet, pmt_packet};

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

        let source = HttpTsSourceSpec::new(
            HttpTsSessionKey::new("unknown", "source", 1),
            "http://127.0.0.1:1/secret?token=never-requested",
            MpegTsRingConfig::new(2, 0).unwrap(),
        );
        assert!(matches!(
            manager.open(source).await,
            Err(SessionStartError::UnknownProvider { pool_id }) if pool_id.as_ref() == "unknown"
        ));

        let diagnostics = SessionDiagnostics::new(HttpTsSessionKey::new("provider", "unit", 1));
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
            Err(SessionFailureKind::Packetization)
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
            Err(SessionFailureKind::Priming)
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
            Err(SessionFailureKind::Priming)
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
            )
            .await,
            SessionFailureKind::Packetization
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
            )
            .await,
            SessionFailureKind::Packetization
        );
        assert_eq!(push_packets(&ring, &diagnostics, &[]), Ok(()));

        assert!(
            recover_session(
                &Client::new(),
                &[],
                0,
                &ring,
                &diagnostics,
                RecoveryPolicy::disabled(),
                Duration::from_millis(10),
            )
            .await
            .is_none()
        );
    }

    fn local_viewer(
        capacity: usize,
        pre_roll: usize,
    ) -> (ViewerHandle, MpegTsRing, Arc<SessionDiagnostics>) {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(capacity, pre_roll).unwrap());
        let diagnostics = SessionDiagnostics::new(HttpTsSessionKey::new("provider", "local", 1));
        diagnostics.set_state(SessionState::Streaming);
        let task = tokio::spawn(std::future::pending::<()>());
        let abort_handle = task.abort_handle();
        drop(task);
        let session = Arc::new(HttpTsSession {
            key: HttpTsSessionKey::new("provider", "local", 1),
            ring: ring.clone(),
            lease_id: 7,
            task: abort_handle,
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
        flaky.add_alternate(HttpTsEndpoint::new(format!(
            "http://{address}/alternate?token=alternate-secret"
        )));
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
        assert_eq!(pool.active_sessions, 2);
        assert_eq!(pool.high_watermark, 2);

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
}
