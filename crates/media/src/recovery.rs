//! Public session diagnostics and bounded recovery policy.

use std::{collections::HashMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use thiserror::Error;

use crate::{HttpTsSessionKey, RingSnapshot, SessionInputAdapter};

pub const MAX_RECOVERY_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    Idle,
    Reserving,
    Starting,
    Priming,
    Streaming,
    Recovering,
    FailingOver,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFailureKind {
    UpstreamEnded,
    Http,
    UpstreamRead,
    ReadTimeout,
    InvalidSync,
    IncompleteTail,
    RingClosed,
    DownstreamLag,
    Packetization,
    Priming,
    RecoveryExpired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryPolicy {
    max_window: Duration,
    retry_delay: Duration,
    keepalive_interval: Duration,
    priming_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RecoveryConfigError {
    #[error("recovery window cannot exceed ten seconds")]
    WindowTooLarge,
    #[error("enabled recovery intervals and priming timeout must be non-zero")]
    ZeroInterval,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_window: MAX_RECOVERY_WINDOW,
            retry_delay: Duration::from_millis(250),
            keepalive_interval: Duration::from_millis(100),
            priming_timeout: Duration::from_secs(2),
        }
    }
}

impl RecoveryPolicy {
    /// Creates a bounded recovery policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the recovery window exceeds ten seconds or an
    /// enabled recovery policy has a zero interval.
    pub fn new(
        max_window: Duration,
        retry_delay: Duration,
        keepalive_interval: Duration,
        priming_timeout: Duration,
    ) -> Result<Self, RecoveryConfigError> {
        if max_window > MAX_RECOVERY_WINDOW {
            return Err(RecoveryConfigError::WindowTooLarge);
        }
        if !max_window.is_zero()
            && (retry_delay.is_zero() || keepalive_interval.is_zero() || priming_timeout.is_zero())
        {
            return Err(RecoveryConfigError::ZeroInterval);
        }
        Ok(Self {
            max_window,
            retry_delay,
            keepalive_interval,
            priming_timeout,
        })
    }

    pub const fn disabled() -> Self {
        Self {
            max_window: Duration::ZERO,
            retry_delay: Duration::from_millis(1),
            keepalive_interval: Duration::from_millis(1),
            priming_timeout: Duration::from_millis(1),
        }
    }

    pub fn max_window(self) -> Duration {
        self.max_window
    }

    pub fn retry_delay(self) -> Duration {
        self.retry_delay
    }

    pub fn keepalive_interval(self) -> Duration {
        self.keepalive_interval
    }

    pub fn priming_timeout(self) -> Duration {
        self.priming_timeout
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpTsSessionSnapshot {
    pub key: HttpTsSessionKey,
    pub channel_name: Option<String>,
    pub state: SessionState,
    pub viewer_count: usize,
    pub upstream_generation: u64,
    pub ring_lag_events: u64,
    pub ring_wrap_events: u64,
    pub overwritten_packets: u64,
    pub reconnect_attempts: u64,
    pub failover_attempts: u64,
    pub failure_count: u64,
    pub first_failure: Option<SessionFailureKind>,
    pub last_failure: Option<SessionFailureKind>,
    pub ring: RingSnapshot,
}

#[derive(Debug)]
pub(crate) struct SessionDiagnostics {
    key: HttpTsSessionKey,
    channel_name: Option<Arc<str>>,
    adapter: Mutex<Option<SessionInputAdapter>>,
    inner: Mutex<DiagnosticsState>,
}

#[derive(Debug)]
struct DiagnosticsState {
    state: SessionState,
    next_viewer_id: u64,
    viewers: HashMap<u64, ViewerPosition>,
    upstream_generation: u64,
    ring_lag_events: u64,
    ring_wrap_events: u64,
    overwritten_packets: u64,
    reconnect_attempts: u64,
    failover_attempts: u64,
    failure_count: u64,
    first_failure: Option<SessionFailureKind>,
    last_failure: Option<SessionFailureKind>,
}

#[derive(Clone, Copy, Debug)]
struct ViewerPosition {
    generation: u64,
    next_sequence: u64,
}

impl SessionDiagnostics {
    pub(crate) fn new(key: HttpTsSessionKey, channel_name: Option<Arc<str>>) -> Arc<Self> {
        Arc::new(Self {
            key,
            channel_name,
            adapter: Mutex::new(None),
            inner: Mutex::new(DiagnosticsState {
                state: SessionState::Idle,
                next_viewer_id: 1,
                viewers: HashMap::new(),
                upstream_generation: 0,
                ring_lag_events: 0,
                ring_wrap_events: 0,
                overwritten_packets: 0,
                reconnect_attempts: 0,
                failover_attempts: 0,
                failure_count: 0,
                first_failure: None,
                last_failure: None,
            }),
        })
    }

    pub(crate) fn set_state(&self, state: SessionState) {
        self.inner.lock().state = state;
    }

    pub(crate) fn key(&self) -> &HttpTsSessionKey {
        &self.key
    }

    pub(crate) fn channel_name(&self) -> Option<&str> {
        self.channel_name.as_deref()
    }

    pub(crate) fn set_adapter(&self, adapter: SessionInputAdapter) {
        *self.adapter.lock() = Some(adapter);
    }

    pub(crate) fn adapter(&self) -> Option<SessionInputAdapter> {
        *self.adapter.lock()
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        self.inner.lock().upstream_generation = generation;
    }

    pub(crate) fn record_write(&self, overwritten_packets: u64) {
        if overwritten_packets == 0 {
            return;
        }
        let mut inner = self.inner.lock();
        inner.ring_wrap_events = inner.ring_wrap_events.saturating_add(1);
        inner.overwritten_packets = inner
            .overwritten_packets
            .saturating_add(overwritten_packets);
    }

    pub(crate) fn record_lag(&self) {
        let mut inner = self.inner.lock();
        inner.ring_lag_events = inner.ring_lag_events.saturating_add(1);
    }

    pub(crate) fn record_reconnect(&self, failover: bool) {
        let mut inner = self.inner.lock();
        inner.reconnect_attempts = inner.reconnect_attempts.saturating_add(1);
        if failover {
            inner.failover_attempts = inner.failover_attempts.saturating_add(1);
        }
    }

    pub(crate) fn record_failure(&self, failure: SessionFailureKind) {
        let mut inner = self.inner.lock();
        inner.failure_count = inner.failure_count.saturating_add(1);
        inner.first_failure.get_or_insert(failure);
        inner.last_failure = Some(failure);
    }

    pub(crate) fn reconnect_attempts(&self) -> u64 {
        self.inner.lock().reconnect_attempts
    }

    pub(crate) fn has_viewers(&self) -> bool {
        !self.inner.lock().viewers.is_empty()
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.inner.lock().state == SessionState::Stopping
    }

    pub(crate) fn register_viewer(
        self: &Arc<Self>,
        generation: u64,
        next_sequence: u64,
    ) -> ViewerRegistration {
        let mut inner = self.inner.lock();
        let id = inner.next_viewer_id;
        inner.next_viewer_id = inner.next_viewer_id.saturating_add(1);
        inner.viewers.insert(
            id,
            ViewerPosition {
                generation,
                next_sequence,
            },
        );
        ViewerRegistration {
            diagnostics: Arc::clone(self),
            id,
        }
    }

    fn update_viewer(&self, id: u64, generation: u64, next_sequence: u64) {
        if let Some(viewer) = self.inner.lock().viewers.get_mut(&id) {
            *viewer = ViewerPosition {
                generation,
                next_sequence,
            };
        }
    }

    pub(crate) fn viewers_drained(&self, generation: u64, target_sequence: u64) -> bool {
        self.inner.lock().viewers.values().all(|viewer| {
            viewer.generation > generation
                || (viewer.generation == generation && viewer.next_sequence >= target_sequence)
        })
    }

    pub(crate) fn snapshot(&self, ring: RingSnapshot) -> HttpTsSessionSnapshot {
        let inner = self.inner.lock();
        HttpTsSessionSnapshot {
            key: self.key.clone(),
            channel_name: self.channel_name.as_deref().map(str::to_owned),
            state: inner.state,
            viewer_count: inner.viewers.len(),
            upstream_generation: inner.upstream_generation,
            ring_lag_events: inner.ring_lag_events,
            ring_wrap_events: inner.ring_wrap_events,
            overwritten_packets: inner.overwritten_packets,
            reconnect_attempts: inner.reconnect_attempts,
            failover_attempts: inner.failover_attempts,
            failure_count: inner.failure_count,
            first_failure: inner.first_failure,
            last_failure: inner.last_failure,
            ring,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewerRegistration {
    diagnostics: Arc<SessionDiagnostics>,
    id: u64,
}

impl ViewerRegistration {
    pub(crate) fn update(&self, generation: u64, next_sequence: u64) {
        self.diagnostics
            .update_viewer(self.id, generation, next_sequence);
    }
}

impl Drop for ViewerRegistration {
    fn drop(&mut self) {
        self.diagnostics.inner.lock().viewers.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_snapshot() -> RingSnapshot {
        RingSnapshot {
            generation: 0,
            first_sequence: 0,
            next_sequence: 4,
            retained_packets: 4,
            capacity_packets: 8,
            closed: None,
        }
    }

    #[test]
    fn recovery_policy_is_bounded_to_ten_seconds() {
        assert_eq!(RecoveryPolicy::default().max_window(), MAX_RECOVERY_WINDOW);
        assert_eq!(
            RecoveryPolicy::new(
                Duration::from_secs(11),
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
            Err(RecoveryConfigError::WindowTooLarge)
        );
        assert_eq!(
            RecoveryPolicy::new(
                Duration::from_secs(1),
                Duration::ZERO,
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
            Err(RecoveryConfigError::ZeroInterval)
        );
        assert!(RecoveryPolicy::disabled().max_window().is_zero());
    }

    #[test]
    fn viewer_positions_and_counters_are_snapshotted() {
        let diagnostics = SessionDiagnostics::new(HttpTsSessionKey::new("pool", "source", 1), None);
        diagnostics.set_state(SessionState::Recovering);
        let first = diagnostics.register_viewer(0, 2);
        let second = diagnostics.register_viewer(0, 4);
        assert!(diagnostics.has_viewers());
        assert!(!diagnostics.viewers_drained(0, 4));
        first.update(0, 4);
        assert!(diagnostics.viewers_drained(0, 4));
        diagnostics.record_write(3);
        diagnostics.record_lag();
        diagnostics.record_reconnect(true);
        diagnostics.record_failure(SessionFailureKind::Http);
        diagnostics.record_failure(SessionFailureKind::ReadTimeout);

        let snapshot = diagnostics.snapshot(ring_snapshot());
        assert_eq!(snapshot.viewer_count, 2);
        assert_eq!(snapshot.state, SessionState::Recovering);
        assert_eq!(snapshot.ring_wrap_events, 1);
        assert_eq!(snapshot.overwritten_packets, 3);
        assert_eq!(snapshot.ring_lag_events, 1);
        assert_eq!(snapshot.reconnect_attempts, 1);
        assert_eq!(snapshot.failover_attempts, 1);
        assert_eq!(snapshot.failure_count, 2);
        assert_eq!(snapshot.first_failure, Some(SessionFailureKind::Http));
        assert_eq!(snapshot.last_failure, Some(SessionFailureKind::ReadTimeout));

        drop(first);
        drop(second);
        assert!(!diagnostics.has_viewers());
        assert_eq!(diagnostics.snapshot(ring_snapshot()).viewer_count, 0);
    }
}
