//! Single-flight construction and weak retention of shared upstream sessions.

use std::{
    fmt,
    future::Future,
    hash::Hash,
    sync::{Arc, Weak},
};

use dashmap::{DashMap, mapref::entry::Entry};
use parking_lot::Mutex;
use tokio::sync::Notify;

/// Deduplicates asynchronous construction of a session for each key.
///
/// The registry retains only a weak reference after initialization. A session
/// therefore disappears naturally when its last viewer/worker drops it, and a
/// later caller can initialize a fresh generation. Initialization of different
/// keys proceeds independently.
pub struct SharedSessionRegistry<K, S, E> {
    entries: Arc<DashMap<K, Arc<SessionEntry<S, E>>>>,
}

impl<K, S, E> Clone for SharedSessionRegistry<K, S, E> {
    fn clone(&self) -> Self {
        Self {
            entries: Arc::clone(&self.entries),
        }
    }
}

impl<K, S, E> fmt::Debug for SharedSessionRegistry<K, S, E>
where
    K: Eq + Hash,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedSessionRegistry")
            .field("entry_count", &self.entries.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct SessionEntry<S, E> {
    state: Mutex<EntryState<S, E>>,
    changed: Notify,
}

#[derive(Debug)]
enum EntryState<S, E> {
    Starting,
    Ready(Weak<S>),
    Failed(E),
    Cancelled,
}

struct InitializerGuard<S, E> {
    entry: Arc<SessionEntry<S, E>>,
    armed: bool,
}

impl<S, E> InitializerGuard<S, E> {
    fn new(entry: Arc<SessionEntry<S, E>>) -> Self {
        Self { entry, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<S, E> Drop for InitializerGuard<S, E> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.entry.state.lock();
        if matches!(*state, EntryState::Starting) {
            *state = EntryState::Cancelled;
            drop(state);
            self.entry.changed.notify_waiters();
        }
    }
}

enum Role {
    Initialize,
    Wait,
}

impl<K, S, E> Default for SharedSessionRegistry<K, S, E>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, S, E> SharedSessionRegistry<K, S, E>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Number of keys currently tracked, including sessions being initialized.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl<K, S, E> SharedSessionRegistry<K, S, E>
where
    K: Clone + Eq + Hash,
    E: Clone,
{
    /// Returns the live session for `key`, without starting one.
    pub fn get(&self, key: &K) -> Option<Arc<S>> {
        let entry = Arc::clone(self.entries.get(key)?.value());
        let state = entry.state.lock();
        match &*state {
            EntryState::Ready(session) => session.upgrade(),
            EntryState::Starting | EntryState::Failed(_) | EntryState::Cancelled => None,
        }
    }

    /// Returns the shared session, initializing it once if necessary.
    ///
    /// Concurrent callers for one key await the same attempt and receive the
    /// same success or clonable error. If the initializing future is cancelled,
    /// a waiter safely takes over rather than waiting forever.
    ///
    /// # Errors
    ///
    /// Returns the initializer's error. Callers attached to that attempt
    /// observe a clone of the same error.
    ///
    /// # Panics
    ///
    /// Panics only if the registry's internal role state becomes inconsistent
    /// and attempts to consume one caller's initializer twice.
    pub async fn get_or_try_init<F, Fut>(&self, key: K, initialize: F) -> Result<Arc<S>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<S, E>>,
    {
        let (entry, mut role) = match self.entries.entry(key.clone()) {
            Entry::Occupied(existing) => (Arc::clone(existing.get()), Role::Wait),
            Entry::Vacant(vacant) => {
                let entry = Arc::new(SessionEntry {
                    state: Mutex::new(EntryState::Starting),
                    changed: Notify::new(),
                });
                vacant.insert(Arc::clone(&entry));
                (entry, Role::Initialize)
            }
        };
        let mut initialize = Some(initialize);

        loop {
            if matches!(role, Role::Initialize) {
                let mut guard = InitializerGuard::new(Arc::clone(&entry));
                let result = initialize
                    .take()
                    .expect("initializer is consumed only by this caller")(
                )
                .await;

                match result {
                    Ok(session) => {
                        let session = Arc::new(session);
                        *entry.state.lock() = EntryState::Ready(Arc::downgrade(&session));
                        guard.disarm();
                        entry.changed.notify_waiters();
                        return Ok(session);
                    }
                    Err(error) => {
                        *entry.state.lock() = EntryState::Failed(error.clone());
                        guard.disarm();
                        entry.changed.notify_waiters();
                        self.remove_entry_if_same(&key, &entry);
                        return Err(error);
                    }
                }
            }

            // Register before observing the state so completion cannot be lost
            // between releasing the mutex and awaiting the notification.
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            role = {
                let mut state = entry.state.lock();
                match &*state {
                    EntryState::Starting => Role::Wait,
                    EntryState::Ready(session) => {
                        if let Some(session) = session.upgrade() {
                            return Ok(session);
                        }
                        *state = EntryState::Starting;
                        Role::Initialize
                    }
                    EntryState::Failed(error) => return Err(error.clone()),
                    EntryState::Cancelled => {
                        *state = EntryState::Starting;
                        Role::Initialize
                    }
                }
            };

            if matches!(role, Role::Wait) {
                changed.await;
            }
        }
    }

    /// Removes dead weak entries. Live or starting sessions are untouched.
    pub fn prune(&self) {
        self.entries.retain(|_, entry| {
            let state = entry.state.lock();
            match &*state {
                EntryState::Ready(session) => session.strong_count() > 0,
                EntryState::Starting => true,
                EntryState::Failed(_) | EntryState::Cancelled => false,
            }
        });
    }

    fn remove_entry_if_same(&self, key: &K, expected: &Arc<SessionEntry<S, E>>) {
        if let Entry::Occupied(existing) = self.entries.entry(key.clone())
            && Arc::ptr_eq(existing.get(), expected)
        {
            existing.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::{
        sync::{Barrier, Notify},
        time::{Duration, sleep, timeout},
    };

    use crate::{AcquireError, ProviderSlotBroker, SlotLease};

    use super::*;

    #[derive(Debug)]
    struct Session {
        value: usize,
        _lease: Option<SlotLease>,
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_share_one_initialization() {
        const CALLERS: usize = 24;
        let registry = Arc::new(SharedSessionRegistry::<String, Session, &'static str>::new());
        let starts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(CALLERS));
        let mut tasks = Vec::new();

        for _ in 0..CALLERS {
            let registry = Arc::clone(&registry);
            let starts = Arc::clone(&starts);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                registry
                    .get_or_try_init("channel-1".to_owned(), || async move {
                        starts.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(25)).await;
                        Ok(Session {
                            value: 42,
                            _lease: None,
                        })
                    })
                    .await
                    .unwrap()
            }));
        }

        let mut sessions = Vec::new();
        for task in tasks {
            sessions.push(task.await.unwrap());
        }
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(sessions.iter().all(|session| session.value == 42));
        assert!(
            sessions
                .iter()
                .all(|session| Arc::ptr_eq(session, &sessions[0]))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_channels_and_six_viewers_create_three_provider_leases() {
        let broker = Arc::new(ProviderSlotBroker::new("shared-pool", 3));
        let registry = Arc::new(SharedSessionRegistry::<String, Session, AcquireError>::new());
        let initialized = Arc::new(AtomicUsize::new(0));
        let acquired = Arc::new(Barrier::new(7));
        let release = Arc::new(Barrier::new(7));
        let mut tasks = Vec::new();

        for viewer in 0..6 {
            let broker = Arc::clone(&broker);
            let registry = Arc::clone(&registry);
            let initialized = Arc::clone(&initialized);
            let acquired = Arc::clone(&acquired);
            let release = Arc::clone(&release);
            tasks.push(tokio::spawn(async move {
                let channel = format!("channel-{}", viewer % 3);
                let session = registry
                    .get_or_try_init(channel.clone(), || async move {
                        initialized.fetch_add(1, Ordering::SeqCst);
                        let lease = broker.try_acquire(channel)?;
                        Ok(Session {
                            value: viewer % 3,
                            _lease: Some(lease),
                        })
                    })
                    .await
                    .unwrap();
                acquired.wait().await;
                release.wait().await;
                session
            }));
        }

        acquired.wait().await;
        assert_eq!(initialized.load(Ordering::SeqCst), 3);
        assert_eq!(broker.snapshot().active_sessions, 3);
        release.wait().await;

        for task in tasks {
            drop(task.await.unwrap());
        }
        assert_eq!(broker.snapshot().active_sessions, 0);
    }

    #[tokio::test]
    async fn expired_weak_session_is_initialized_again() {
        let registry = SharedSessionRegistry::<String, Session, &'static str>::new();
        let starts = Arc::new(AtomicUsize::new(0));

        for expected in 1..=2 {
            let starts_for_init = Arc::clone(&starts);
            let session = registry
                .get_or_try_init("channel".to_owned(), || async move {
                    let value = starts_for_init.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(Session {
                        value,
                        _lease: None,
                    })
                })
                .await
                .unwrap();
            assert_eq!(session.value, expected);
            drop(session);
        }
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        registry.prune();
        assert_eq!(registry.entry_count(), 0);
    }

    #[tokio::test]
    async fn failed_attempt_is_shared_and_a_later_call_can_retry() {
        let registry = Arc::new(SharedSessionRegistry::<String, Session, &'static str>::new());
        let started = Arc::new(Notify::new());
        let allow_failure = Arc::new(Notify::new());

        let leader = {
            let registry = Arc::clone(&registry);
            let started = Arc::clone(&started);
            let allow_failure = Arc::clone(&allow_failure);
            tokio::spawn(async move {
                registry
                    .get_or_try_init("channel".to_owned(), || async move {
                        started.notify_one();
                        allow_failure.notified().await;
                        Err("upstream failed")
                    })
                    .await
            })
        };
        started.notified().await;
        let waiter = {
            let registry = Arc::clone(&registry);
            tokio::spawn(async move {
                registry
                    .get_or_try_init("channel".to_owned(), || async {
                        panic!("waiter must not initialize while leader is active")
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        allow_failure.notify_one();

        assert!(matches!(leader.await.unwrap(), Err("upstream failed")));
        assert!(matches!(waiter.await.unwrap(), Err("upstream failed")));

        let recovered = registry
            .get_or_try_init("channel".to_owned(), || async {
                Ok(Session {
                    value: 7,
                    _lease: None,
                })
            })
            .await
            .unwrap();
        assert_eq!(recovered.value, 7);
    }

    #[tokio::test]
    async fn cancellation_hands_initialization_to_a_waiter() {
        let registry = Arc::new(SharedSessionRegistry::<String, Session, &'static str>::new());
        let started = Arc::new(Notify::new());

        let leader = {
            let registry = Arc::clone(&registry);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                registry
                    .get_or_try_init("channel".to_owned(), || async move {
                        started.notify_one();
                        std::future::pending::<Result<Session, &'static str>>().await
                    })
                    .await
            })
        };
        started.notified().await;

        let waiter = {
            let registry = Arc::clone(&registry);
            tokio::spawn(async move {
                registry
                    .get_or_try_init("channel".to_owned(), || async {
                        Ok(Session {
                            value: 99,
                            _lease: None,
                        })
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        leader.abort();

        let recovered = timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(recovered.value, 99);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn different_keys_initialize_concurrently() {
        let registry = Arc::new(SharedSessionRegistry::<String, Session, &'static str>::new());
        let inside_initializers = Arc::new(Barrier::new(2));
        let mut tasks = Vec::new();

        for value in 0..2 {
            let registry = Arc::clone(&registry);
            let inside_initializers = Arc::clone(&inside_initializers);
            tasks.push(tokio::spawn(async move {
                registry
                    .get_or_try_init(format!("channel-{value}"), || async move {
                        inside_initializers.wait().await;
                        Ok(Session {
                            value,
                            _lease: None,
                        })
                    })
                    .await
                    .unwrap()
            }));
        }

        timeout(Duration::from_secs(1), async {
            for task in tasks {
                task.await.unwrap();
            }
        })
        .await
        .expect("different keys must not block each other");
    }
}
