//! In-memory provider connection accounting.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Weak},
};

use parking_lot::Mutex;
use thiserror::Error;

/// A connection-slot broker scoped to one provider account or shared pool.
///
/// Acquiring the same session key is idempotent: all returned handles refer to
/// one allocation and consume one slot until the final handle is dropped.
#[derive(Clone)]
pub struct ProviderSlotBroker {
    inner: Arc<BrokerInner>,
}

impl fmt::Debug for ProviderSlotBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSlotBroker")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

#[derive(Debug)]
struct BrokerInner {
    pool_id: Arc<str>,
    state: Mutex<BrokerState>,
}

#[derive(Debug)]
struct BrokerState {
    capacity: usize,
    high_watermark: usize,
    next_lease_id: u64,
    sessions: HashMap<Arc<str>, Weak<LeaseInner>>,
}

/// A reference-counted allocation from a [`ProviderSlotBroker`].
///
/// Cloning a lease does not consume another provider connection. The slot is
/// released only after the final clone is dropped.
#[derive(Clone)]
pub struct SlotLease {
    inner: Arc<LeaseInner>,
}

impl fmt::Debug for SlotLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlotLease")
            .field("pool_id", &self.inner.pool_id)
            .field("session_key", &self.inner.session_key)
            .field("lease_id", &self.inner.lease_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct LeaseInner {
    pool_id: Arc<str>,
    session_key: Arc<str>,
    lease_id: u64,
    broker: Weak<BrokerInner>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let Some(broker) = self.broker.upgrade() else {
            return;
        };

        let mut state = broker.state.lock();
        let should_remove = state
            .sessions
            .get(&self.session_key)
            .is_some_and(|lease| lease.upgrade().is_none());

        if should_remove {
            state.sessions.remove(&self.session_key);
        }
    }
}

/// Current broker accounting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolSnapshot {
    pub pool_id: Arc<str>,
    pub capacity: usize,
    pub active_sessions: usize,
    pub high_watermark: usize,
    pub available_slots: usize,
}

/// A unique session could not acquire a provider slot.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AcquireError {
    #[error(
        "provider pool {pool_id:?} is at capacity ({active_sessions}/{capacity} active sessions)"
    )]
    AtCapacity {
        pool_id: Arc<str>,
        capacity: usize,
        active_sessions: usize,
    },
    #[error("provider pool exhausted its internal lease identifier space")]
    LeaseIdExhausted,
}

impl ProviderSlotBroker {
    /// Creates a broker for one provider account or explicitly shared pool.
    pub fn new(pool_id: impl Into<Arc<str>>, capacity: usize) -> Self {
        Self {
            inner: Arc::new(BrokerInner {
                pool_id: pool_id.into(),
                state: Mutex::new(BrokerState {
                    capacity,
                    high_watermark: 0,
                    next_lease_id: 1,
                    sessions: HashMap::new(),
                }),
            }),
        }
    }

    /// Returns an existing allocation for `session_key`, or reserves a new one.
    ///
    /// The capacity check and insertion occur under the same lock, so concurrent
    /// callers cannot oversubscribe the provider pool.
    ///
    /// # Errors
    ///
    /// Returns [`AcquireError::AtCapacity`] for a new session when the pool is
    /// full, or [`AcquireError::LeaseIdExhausted`] if its identifier space ends.
    pub fn try_acquire(&self, session_key: impl Into<Arc<str>>) -> Result<SlotLease, AcquireError> {
        let session_key = session_key.into();
        let mut state = self.inner.state.lock();

        // A lease normally removes itself synchronously on final drop. Retain
        // also covers the narrow race where that destructor is waiting for this
        // mutex, preventing a false capacity rejection.
        state.sessions.retain(|_, lease| lease.strong_count() > 0);

        if let Some(existing) = state.sessions.get(&session_key) {
            if let Some(existing) = existing.upgrade() {
                return Ok(SlotLease { inner: existing });
            }
            state.sessions.remove(&session_key);
        }

        if state.sessions.len() >= state.capacity {
            return Err(AcquireError::AtCapacity {
                pool_id: Arc::clone(&self.inner.pool_id),
                capacity: state.capacity,
                active_sessions: state.sessions.len(),
            });
        }

        let lease_id = state.next_lease_id;
        state.next_lease_id = state
            .next_lease_id
            .checked_add(1)
            .ok_or(AcquireError::LeaseIdExhausted)?;

        let lease = Arc::new(LeaseInner {
            pool_id: Arc::clone(&self.inner.pool_id),
            session_key: Arc::clone(&session_key),
            lease_id,
            broker: Arc::downgrade(&self.inner),
        });
        state.sessions.insert(session_key, Arc::downgrade(&lease));
        state.high_watermark = state.high_watermark.max(state.sessions.len());

        Ok(SlotLease { inner: lease })
    }

    /// Changes the cap without terminating existing allocations.
    ///
    /// Lowering the cap below current usage only prevents new unique sessions;
    /// idempotent acquisitions of existing sessions continue to succeed.
    pub fn set_capacity(&self, capacity: usize) {
        self.inner.state.lock().capacity = capacity;
    }

    pub fn snapshot(&self) -> PoolSnapshot {
        let mut state = self.inner.state.lock();
        state.sessions.retain(|_, lease| lease.strong_count() > 0);
        PoolSnapshot {
            pool_id: Arc::clone(&self.inner.pool_id),
            capacity: state.capacity,
            active_sessions: state.sessions.len(),
            high_watermark: state.high_watermark,
            available_slots: state.capacity.saturating_sub(state.sessions.len()),
        }
    }
}

impl SlotLease {
    pub fn pool_id(&self) -> &str {
        &self.inner.pool_id
    }

    pub fn session_key(&self) -> &str {
        &self.inner.session_key
    }

    /// Stable within this broker process and allocation lifetime.
    pub fn lease_id(&self) -> u64 {
        self.inner.lease_id
    }

    /// Returns whether both handles share the same provider allocation.
    pub fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    #[test]
    fn reacquiring_a_session_is_idempotent_until_the_last_drop() {
        let broker = ProviderSlotBroker::new("account-a", 1);
        let first = broker.try_acquire("channel-1").unwrap();
        let second = broker.try_acquire("channel-1").unwrap();

        assert!(first.same_allocation(&second));
        assert_eq!(first.lease_id(), second.lease_id());
        assert_eq!(broker.snapshot().active_sessions, 1);
        assert!(matches!(
            broker.try_acquire("channel-2"),
            Err(AcquireError::AtCapacity { .. })
        ));

        drop(first);
        assert_eq!(broker.snapshot().active_sessions, 1);
        drop(second);
        assert_eq!(broker.snapshot().active_sessions, 0);
        assert!(broker.try_acquire("channel-2").is_ok());
    }

    #[test]
    fn three_channels_and_six_viewers_consume_three_leases() {
        let broker = Arc::new(ProviderSlotBroker::new("shared-provider-pool", 3));
        let ready = Arc::new(Barrier::new(7));
        let release = Arc::new(Barrier::new(7));
        let mut workers = Vec::new();

        for viewer in 0..6 {
            let broker = Arc::clone(&broker);
            let ready = Arc::clone(&ready);
            let release = Arc::clone(&release);
            workers.push(thread::spawn(move || {
                let channel = format!("channel-{}", viewer % 3);
                let lease = broker.try_acquire(channel).unwrap();
                ready.wait();
                release.wait();
                (viewer % 3, lease.lease_id())
            }));
        }

        ready.wait();
        assert_eq!(broker.snapshot().active_sessions, 3);
        assert_eq!(broker.snapshot().high_watermark, 3);
        release.wait();

        let pairs: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        for channel in 0..3 {
            let ids: HashSet<_> = pairs
                .iter()
                .filter_map(|(actual_channel, id)| (*actual_channel == channel).then_some(*id))
                .collect();
            assert_eq!(ids.len(), 1, "each channel must share one lease");
        }
        assert_eq!(broker.snapshot().active_sessions, 0);
    }

    #[test]
    fn concurrent_unique_sessions_never_oversubscribe() {
        const CAPACITY: usize = 4;
        const CONTENDERS: usize = 32;

        let broker = Arc::new(ProviderSlotBroker::new("account-a", CAPACITY));
        let start = Arc::new(Barrier::new(CONTENDERS + 1));
        let (leases_tx, leases_rx) = std::sync::mpsc::channel();
        let mut workers = Vec::new();

        for id in 0..CONTENDERS {
            let broker = Arc::clone(&broker);
            let start = Arc::clone(&start);
            let leases_tx = leases_tx.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                if let Ok(lease) = broker.try_acquire(format!("session-{id}")) {
                    leases_tx.send(lease).unwrap();
                }
            }));
        }
        drop(leases_tx);

        start.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let leases: Vec<_> = leases_rx.into_iter().collect();
        assert_eq!(leases.len(), CAPACITY);
        assert_eq!(broker.snapshot().active_sessions, CAPACITY);
        assert_eq!(broker.snapshot().high_watermark, CAPACITY);
        drop(leases);
        assert_eq!(broker.snapshot().active_sessions, 0);
    }

    #[test]
    fn lowering_capacity_does_not_revoke_existing_sessions() {
        let broker = ProviderSlotBroker::new("account-a", 2);
        let first = broker.try_acquire("first").unwrap();
        let second = broker.try_acquire("second").unwrap();

        broker.set_capacity(1);
        assert_eq!(broker.snapshot().available_slots, 0);
        assert!(broker.try_acquire("first").unwrap().same_allocation(&first));
        assert!(matches!(
            broker.try_acquire("third"),
            Err(AcquireError::AtCapacity {
                capacity: 1,
                active_sessions: 2,
                ..
            })
        ));

        drop(first);
        drop(second);
    }

    #[test]
    fn a_zero_capacity_pool_only_rejects_new_sessions() {
        let broker = ProviderSlotBroker::new("disabled", 0);
        assert!(matches!(
            broker.try_acquire("first"),
            Err(AcquireError::AtCapacity {
                capacity: 0,
                active_sessions: 0,
                ..
            })
        ));
    }
}
