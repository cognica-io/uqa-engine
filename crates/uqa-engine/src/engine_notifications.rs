//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional SQL asynchronous-notification coordination.

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod cross_process;
mod hub;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
mod cross_process {
    use uqa_sql::SQLError;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(super) struct CrossProcessQueueState {
        pub(super) next_sequence: u64,
        pub(super) head_position: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct CrossProcessQueueEntry {
        pub(super) sequence: u64,
        pub(super) process_id: i32,
        pub(super) channel: String,
        pub(super) payload: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct CrossProcessListenerRow {
        pub(super) owner_id: [u8; 16],
        pub(super) session_id: u64,
        pub(super) process_id: i32,
        pub(super) wake_port: u16,
        pub(super) channels: Vec<String>,
        pub(super) transaction_open: bool,
        pub(super) next_sequence: u64,
        pub(super) position: u64,
    }

    pub(super) struct ListenerLease {
        owner_id: [u8; 16],
    }

    impl ListenerLease {
        pub(super) const fn owner_id(&self) -> [u8; 16] {
            self.owner_id
        }
    }

    pub(super) struct CrossProcessRegistryTransaction;

    impl CrossProcessRegistryTransaction {
        pub(super) fn allocate_backend_process_id(&self) -> Result<i32, SQLError> {
            Err(unsupported())
        }

        pub(super) fn queue_state(&self) -> Result<CrossProcessQueueState, SQLError> {
            Err(unsupported())
        }

        pub(super) fn save_queue_state(
            &self,
            _state: CrossProcessQueueState,
        ) -> Result<(), SQLError> {
            Err(unsupported())
        }

        pub(super) fn append_entries(
            &self,
            _entries: &[CrossProcessQueueEntry],
        ) -> Result<(), SQLError> {
            Err(unsupported())
        }

        pub(super) fn entries_from(
            &self,
            _from_sequence: u64,
        ) -> Result<Vec<CrossProcessQueueEntry>, SQLError> {
            Err(unsupported())
        }

        pub(super) fn delete_entries_before(&self, _sequence: u64) -> Result<(), SQLError> {
            Err(unsupported())
        }

        pub(super) fn listeners(&self) -> Result<Vec<CrossProcessListenerRow>, SQLError> {
            Err(unsupported())
        }

        pub(super) fn save_listener(
            &self,
            _listener: &CrossProcessListenerRow,
        ) -> Result<(), SQLError> {
            Err(unsupported())
        }

        pub(super) fn drop_listener(
            &self,
            _owner_id: [u8; 16],
            _session_id: u64,
        ) -> Result<(), SQLError> {
            Err(unsupported())
        }

        pub(super) fn commit(self) -> Result<(), SQLError> {
            Err(unsupported())
        }
    }

    pub(super) struct CrossProcessCoordinator;

    impl CrossProcessCoordinator {
        pub(super) fn begin_registry_transaction(
            &self,
        ) -> Result<CrossProcessRegistryTransaction, SQLError> {
            Err(unsupported())
        }

        pub(super) fn create_listener_lease(&self) -> Result<ListenerLease, SQLError> {
            Err(unsupported())
        }

        pub(super) fn listener_is_alive(
            &self,
            _owner_id: [u8; 16],
            _local_owner_ids: &[[u8; 16]],
        ) -> Result<bool, SQLError> {
            Err(unsupported())
        }

        pub(super) const fn wake_port(&self) -> u16 {
            0
        }

        pub(super) fn wake(_ports: &[u16]) {}
    }

    fn unsupported() -> SQLError {
        SQLError::Internal(
            "cross-process asynchronous notifications are unavailable on this target".into(),
        )
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use crate::Engine;
use cross_process::{
    CrossProcessCoordinator, CrossProcessListenerRow, CrossProcessQueueEntry,
    CrossProcessQueueState, CrossProcessRegistryTransaction, ListenerLease,
};
use parking_lot::{Condvar, Mutex, MutexGuard};
use uqa_sql::SQLError;

const MAX_NOTIFICATION_CHANNEL_BYTES: usize = 64;
const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 8_000;
const NOTIFICATION_QUEUE_PAGE_BYTES: u64 = 8_192;
const MAX_NOTIFICATION_QUEUE_PAGES: u64 = 1_048_576;
const NOTIFICATION_ENTRY_HEADER_BYTES: u64 = 16;
const MIN_NOTIFICATION_ENTRY_BYTES: u64 = 20;
const NOTIFICATION_QUEUE_WARNING_INTERVAL: Duration = Duration::from_secs(5);

/// One committed SQL notification waiting for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLNotification {
    /// Stable backend process identifier of the sending SQL session.
    pub process_id: i32,
    /// Subscribed SQL channel that received the message.
    pub channel: String,
    /// Sender-provided payload, or the empty string when `NOTIFY` omitted it.
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingNotification {
    pub(crate) channel: String,
    pub(crate) payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingListenAction {
    Listen(String),
    Unlisten(String),
    UnlistenAll,
}

impl PendingListenAction {
    fn apply(&self, channels: &mut Vec<String>) {
        match self {
            Self::Listen(channel) if !channels.contains(channel) => channels.push(channel.clone()),
            Self::Listen(_) => {}
            Self::Unlisten(channel) => channels.retain(|candidate| candidate != channel),
            Self::UnlistenAll => channels.clear(),
        }
    }
}

#[derive(Clone)]
struct CommittedNotification {
    sequence: u64,
    process_id: i32,
    channel: String,
    payload: String,
}

struct NotificationListener {
    process_id: i32,
    channels: Vec<String>,
    queue: Weak<Mutex<VecDeque<SQLNotification>>>,
    wake: Weak<Condvar>,
    next_sequence: u64,
    position: u64,
    transaction_open: bool,
    lease: Option<ListenerLease>,
}

struct NotificationSessionCommit<'a> {
    session_id: u64,
    process_id: i32,
    channels: Vec<String>,
    queue: &'a Arc<Mutex<VecDeque<SQLNotification>>>,
    wake: &'a Arc<Condvar>,
    notices: &'a Arc<Mutex<Vec<(String, String)>>>,
    pending: &'a [PendingNotification],
}

struct PreparedDelivery {
    session_id: u64,
    notifications: Vec<SQLNotification>,
}

struct CrossNotificationCommit {
    registry: Option<CrossProcessRegistryTransaction>,
    new_lease: Option<ListenerLease>,
    deliveries: Vec<PreparedDelivery>,
    wake_ports: Vec<u16>,
    warning: Option<String>,
}

struct PreparedCrossSubscription {
    new_lease: Option<ListenerLease>,
    owner_id: Option<[u8; 16]>,
    listeners: Vec<CrossProcessListenerRow>,
}

struct CrossProcessState {
    database_path: std::path::PathBuf,
    hub: Weak<NotificationHub>,
    coordinator: Mutex<Option<Arc<CrossProcessCoordinator>>>,
}

impl CrossProcessState {
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    fn allocate_backend_process_id(&self) -> Result<i32, SQLError> {
        CrossProcessCoordinator::allocate_backend_process_id_for_database(&self.database_path)
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    fn allocate_backend_process_id(&self) -> Result<i32, SQLError> {
        Err(SQLError::Internal(
            "cross-process backend process identifiers are unavailable on this target".into(),
        ))
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    fn coordinator(&self) -> Result<Arc<CrossProcessCoordinator>, SQLError> {
        let mut initialized = self.coordinator.lock();
        if let Some(coordinator) = initialized.as_ref() {
            return Ok(Arc::clone(coordinator));
        }
        let (coordinator, listener) =
            CrossProcessCoordinator::open(&self.database_path).map_err(SQLError::Internal)?;
        let coordinator = Arc::new(coordinator);
        coordinator
            .start_worker(listener, self.hub.clone())
            .map_err(SQLError::Internal)?;
        *initialized = Some(Arc::clone(&coordinator));
        Ok(coordinator)
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    fn coordinator(&self) -> Result<Arc<CrossProcessCoordinator>, SQLError> {
        Err(SQLError::Internal(
            "cross-process asynchronous notifications are unavailable on this target".into(),
        ))
    }
}

pub(super) struct NotificationCommitGuard<'a> {
    _gate: MutexGuard<'a, ()>,
    cross: Option<CrossNotificationCommit>,
}

#[derive(Default)]
struct NotificationHubState {
    listeners: BTreeMap<u64, NotificationListener>,
    entries: VecDeque<CommittedNotification>,
    next_sequence: u64,
    head_position: u64,
    last_queue_warning: Option<Instant>,
}

pub(crate) struct NotificationHub {
    commit_gate: Mutex<()>,
    state: Mutex<NotificationHubState>,
    max_queue_pages: u64,
    cross: Option<CrossProcessState>,
    cross_error: Mutex<Option<String>>,
}

impl Default for NotificationHub {
    fn default() -> Self {
        Self {
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: MAX_NOTIFICATION_QUEUE_PAGES,
            cross: None,
            cross_error: Mutex::new(None),
        }
    }
}

fn append_notification(
    state: &mut NotificationHubState,
    process_id: i32,
    notification: &PendingNotification,
) {
    let end_position = notification_end_position(state.head_position, notification);
    let sequence = state.next_sequence;
    state.entries.push_back(CommittedNotification {
        sequence,
        process_id,
        channel: notification.channel.clone(),
        payload: notification.payload.clone(),
    });
    state.next_sequence = state.next_sequence.saturating_add(1);
    state.head_position = end_position;
}

fn notification_end_position(position: u64, notification: &PendingNotification) -> u64 {
    let content = NOTIFICATION_ENTRY_HEADER_BYTES
        .saturating_add(notification.channel.len() as u64)
        .saturating_add(1)
        .saturating_add(notification.payload.len() as u64)
        .saturating_add(1);
    let length = content.saturating_add(3) & !3;
    let offset = position % NOTIFICATION_QUEUE_PAGE_BYTES;
    let aligned_position = if offset.saturating_add(length) > NOTIFICATION_QUEUE_PAGE_BYTES {
        position.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - offset)
    } else {
        position
    };
    let end = aligned_position.saturating_add(length);
    let end_offset = end % NOTIFICATION_QUEUE_PAGE_BYTES;
    if end_offset.saturating_add(MIN_NOTIFICATION_ENTRY_BYTES) > NOTIFICATION_QUEUE_PAGE_BYTES {
        end.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - end_offset)
    } else {
        end
    }
}

fn notifications_fit_queue(
    mut head: u64,
    tail: u64,
    max_queue_pages: u64,
    pending: &[PendingNotification],
) -> bool {
    let tail_page = queue_page(tail);
    for notification in pending {
        if queue_page(head).saturating_sub(tail_page) >= max_queue_pages {
            return false;
        }
        let content = NOTIFICATION_ENTRY_HEADER_BYTES
            .saturating_add(notification.channel.len() as u64)
            .saturating_add(1)
            .saturating_add(notification.payload.len() as u64)
            .saturating_add(1);
        let length = content.saturating_add(3) & !3;
        let offset = head % NOTIFICATION_QUEUE_PAGE_BYTES;
        if offset.saturating_add(length) > NOTIFICATION_QUEUE_PAGE_BYTES {
            head = head.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - offset);
            if queue_page(head).saturating_sub(tail_page) >= max_queue_pages {
                return false;
            }
        }
        head = notification_end_position(head, notification);
    }
    true
}

fn projected_tail_position(
    state: &NotificationHubState,
    session_id: u64,
    final_channels: &[String],
) -> u64 {
    state
        .listeners
        .iter()
        .filter_map(|(listener_id, listener)| {
            (*listener_id != session_id || !final_channels.is_empty()).then_some(listener.position)
        })
        .min()
        .unwrap_or(state.head_position)
}

fn queue_page(position: u64) -> u64 {
    position / NOTIFICATION_QUEUE_PAGE_BYTES
}

fn queue_usage(state: &NotificationHubState, max_queue_pages: u64) -> f64 {
    let tail = state
        .listeners
        .values()
        .map(|listener| listener.position)
        .min()
        .unwrap_or(state.head_position);
    let pages = queue_page(state.head_position).saturating_sub(queue_page(tail));
    pages as f64 / max_queue_pages as f64
}

fn validate_channel(channel: &str) -> Result<(), SQLError> {
    if channel.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "channel name cannot be empty".into(),
        });
    }
    if channel.len() >= MAX_NOTIFICATION_CHANNEL_BYTES {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "channel name too long".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum NotificationHubIdentity {
    Durable(uqa_storage::PersistentStorageIdentity),
    Provider(usize),
}

static DATABASE_NOTIFICATION_HUBS: OnceLock<
    Mutex<HashMap<NotificationHubIdentity, Weak<NotificationHub>>>,
> = OnceLock::new();

#[derive(Default)]
struct BackendProcessIdRegistry {
    active: BTreeSet<i32>,
    next: i32,
}

static BACKEND_PROCESS_IDS: OnceLock<Mutex<BackendProcessIdRegistry>> = OnceLock::new();

pub(crate) fn allocate_backend_process_id() -> i32 {
    let registry = BACKEND_PROCESS_IDS.get_or_init(|| {
        Mutex::new(BackendProcessIdRegistry {
            active: BTreeSet::new(),
            next: 1,
        })
    });
    let mut registry = registry.lock();
    for _ in 0..i32::MAX {
        let candidate = registry.next;
        registry.next = registry.next.checked_add(1).unwrap_or(1);
        if registry.active.insert(candidate) {
            return candidate;
        }
    }
    panic!("exhausted positive backend process identifiers");
}

pub(crate) fn release_backend_process_id(process_id: i32) {
    if let Some(registry) = BACKEND_PROCESS_IDS.get() {
        registry.lock().active.remove(&process_id);
    }
}

pub(crate) fn shared_provider_notification_hub(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    provider: &Arc<dyn uqa_storage::PersistentStorageProvider>,
) -> Arc<NotificationHub> {
    shared_notification_hub(
        identity,
        NotificationHubIdentity::Provider(Arc::as_ptr(provider).cast::<()>() as usize),
    )
}

pub(crate) fn shared_backend_notification_hub(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    backend: &Arc<dyn uqa_storage::PersistentStorageBackend>,
) -> Arc<NotificationHub> {
    shared_notification_hub(
        identity,
        NotificationHubIdentity::Provider(Arc::as_ptr(backend).cast::<()>() as usize),
    )
}

fn shared_notification_hub(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    fallback: NotificationHubIdentity,
) -> Arc<NotificationHub> {
    let identity = identity.map_or(fallback, NotificationHubIdentity::Durable);
    let registry = DATABASE_NOTIFICATION_HUBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock();
    registry.retain(|_, hub| hub.strong_count() != 0);
    if let Some(hub) = registry.get(&identity).and_then(Weak::upgrade) {
        return hub;
    }
    let hub = match &identity {
        NotificationHubIdentity::Durable(uqa_storage::PersistentStorageIdentity::File(path)) => {
            NotificationHub::for_database_file(path)
        }
        _ => Arc::new(NotificationHub::default()),
    };
    registry.insert(identity, Arc::downgrade(&hub));
    hub
}

impl Engine {
    pub(crate) fn listen(&self, channel: &str) -> Result<(), SQLError> {
        validate_channel(channel)?;
        self.pending_listen_action(PendingListenAction::Listen(channel.to_string()))
    }

    pub(crate) fn unlisten(&self, channel: Option<&str>) -> Result<(), SQLError> {
        let action = match channel {
            Some(channel) => {
                validate_channel(channel)?;
                PendingListenAction::Unlisten(channel.to_string())
            }
            None => PendingListenAction::UnlistenAll,
        };
        self.pending_listen_action(action)
    }

    fn pending_listen_action(&self, action: PendingListenAction) -> Result<(), SQLError> {
        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("LISTEN state changed without a transaction frame".into())
        })?;
        frame.pending_listen_actions.push(action);
        Ok(())
    }

    pub(crate) fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError> {
        validate_channel(channel)?;
        if payload.len() >= MAX_NOTIFICATION_PAYLOAD_BYTES {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: "payload string too long".into(),
            });
        }
        let notification = PendingNotification {
            channel: channel.to_string(),
            payload: payload.to_string(),
        };
        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("NOTIFY executed without a transaction frame".into())
        })?;
        if !frame.pending_notifications.contains(&notification) {
            frame.pending_notifications.push(notification);
        }
        Ok(())
    }

    pub(super) fn begin_notification_transaction(&self) -> Result<(), SQLError> {
        self.notification_hub.begin_transaction(self.session_id)
    }

    pub(super) fn begin_notification_commit<'a>(
        &'a self,
        outer: bool,
        transaction: &crate::TransactionFrame,
    ) -> Result<Option<NotificationCommitGuard<'a>>, SQLError> {
        if !outer {
            return Ok(None);
        }
        let current_channels = self.session.state.read().listened_channels.clone();
        if current_channels.is_empty()
            && transaction.pending_listen_actions.is_empty()
            && transaction.pending_notifications.is_empty()
        {
            return Ok(None);
        }
        let final_channels = transaction.final_listened_channels(&current_channels);
        let cross = self
            .notification_hub
            .cross
            .as_ref()
            .map(CrossProcessState::coordinator)
            .transpose()?;
        let registry = cross
            .as_ref()
            .map(|cross| cross.begin_registry_transaction())
            .transpose()?;
        let commit = self.notification_hub.commit_gate.lock();
        let prepared = if let (Some(cross), Some(registry)) = (cross, registry) {
            Some(self.notification_hub.prepare_cross_commit(
                &cross,
                registry,
                self.session_id,
                self.backend_process_id(),
                &final_channels,
                &transaction.pending_notifications,
            )?)
        } else {
            self.notification_hub.validate_commit(
                &commit,
                self.session_id,
                &final_channels,
                &transaction.pending_notifications,
            )?;
            None
        };
        Ok(Some(NotificationCommitGuard {
            _gate: commit,
            cross: prepared,
        }))
    }

    pub(super) fn commit_notification_state(
        &self,
        commit: NotificationCommitGuard<'_>,
        transaction: &crate::TransactionFrame,
    ) -> Result<(), SQLError> {
        let current_channels = self.session.state.read().listened_channels.clone();
        let channels = transaction.final_listened_channels(&current_channels);
        let session = NotificationSessionCommit {
            session_id: self.session_id,
            process_id: self.backend_process_id(),
            channels: channels.clone(),
            queue: &self.runtime.notifications,
            wake: &self.runtime.notification_wake,
            notices: &self.runtime.notices,
            pending: &transaction.pending_notifications,
        };
        let NotificationCommitGuard { _gate: gate, cross } = commit;
        match cross {
            Some(prepared) => {
                if let Err(error) = self
                    .notification_hub
                    .finalize_cross_commit(gate, prepared, session)
                {
                    return Err(match self.notification_hub.rollback_session(self.session_id) {
                        Ok(()) => error,
                        Err(recovery_error) => SQLError::Internal(format!(
                            "{error}; restore asynchronous notification listener after commit failure: {recovery_error}"
                        )),
                    });
                }
            }
            None => self.notification_hub.commit_session(&gate, session),
        }
        self.session.state.write().listened_channels = channels;
        Ok(())
    }

    pub(super) fn rollback_notification_state(&self) -> Result<(), SQLError> {
        self.notification_hub.rollback_session(self.session_id)
    }

    pub(crate) fn clear_notification_listener_without_transaction(&self) -> Result<(), SQLError> {
        self.notification_hub.replace_idle_session(
            self.session_id,
            self.backend_process_id(),
            Vec::new(),
            &self.runtime.notifications,
            &self.runtime.notification_wake,
            &self.runtime.notices,
        )?;
        self.session.state.write().listened_channels.clear();
        Ok(())
    }

    /// PostgreSQL-compatible backend process identifier for this logical SQL session.
    #[must_use]
    pub fn backend_process_id(&self) -> i32 {
        self.session
            .backend_process_id
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn listening_channels(&self) -> Vec<String> {
        self.session.state.read().listened_channels.clone()
    }

    pub(crate) fn notification_queue_usage(&self) -> Result<f64, SQLError> {
        self.notification_hub.usage()
    }

    /// Pull committed cross-process messages into this session without draining them.
    pub fn poll_sql_notifications(&self) -> Result<usize, SQLError> {
        if self.transaction_depth() != 0 {
            return Ok(0);
        }
        self.notification_hub
            .try_synchronize_cross_process_session(Some((self.session_id, false)))?;
        if let Some(error) = self.notification_hub.cross_error.lock().take() {
            return Err(SQLError::Internal(error));
        }
        Ok(self.runtime.notifications.lock().len())
    }

    /// Wait until at least one committed notification is ready for this idle SQL session or the timeout elapses.
    pub fn wait_for_sql_notifications(&self, timeout: Duration) -> Result<bool, SQLError> {
        if self.transaction_depth() != 0 {
            return Ok(false);
        }
        let started = Instant::now();
        loop {
            if self.poll_sql_notifications()? != 0 {
                return Ok(true);
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(false);
            }
            let mut notifications = self.runtime.notifications.lock();
            if !notifications.is_empty() {
                return Ok(true);
            }
            self.runtime
                .notification_wake
                .wait_for(&mut notifications, timeout.saturating_sub(elapsed));
        }
    }

    /// Drain committed notifications available to this SQL session. Notifications received during a transaction remain queued until that transaction ends.
    pub fn take_sql_notifications(&self) -> Vec<SQLNotification> {
        if self.transaction_depth() != 0 {
            return Vec::new();
        }
        self.runtime.notifications.lock().drain(..).collect()
    }
}

impl crate::TransactionFrame {
    fn final_listened_channels(&self, current: &[String]) -> Vec<String> {
        let mut channels = current.to_vec();
        for action in &self.pending_listen_actions {
            action.apply(&mut channels);
        }
        channels
    }

    pub(super) fn merge_pending_listen_actions(&mut self, pending: Vec<PendingListenAction>) {
        self.pending_listen_actions.extend(pending);
    }

    pub(super) fn merge_pending_notifications(&mut self, pending: Vec<PendingNotification>) {
        for notification in pending {
            if !self.pending_notifications.contains(&notification) {
                self.pending_notifications.push(notification);
            }
        }
    }

    pub(super) fn restore_pending_notification_savepoint(&mut self, position: usize) {
        self.pending_listen_actions = self.savepoints[position].pending_listen_actions.clone();
        self.pending_notifications = self.savepoints[position].pending_notifications.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(channel_bytes: usize, payload_bytes: usize) -> PendingNotification {
        PendingNotification {
            channel: "c".repeat(channel_bytes),
            payload: "p".repeat(payload_bytes),
        }
    }

    #[test]
    fn queue_layout_accounts_for_alignment_page_padding_and_capacity() {
        let largest = pending(63, 7_999);
        let smallest = pending(1, 0);
        assert_eq!(notification_end_position(0, &largest), 8_080);
        assert_eq!(notification_end_position(8_080, &smallest), 8_100);

        let mut one_page = vec![largest];
        one_page.extend(std::iter::repeat_n(smallest.clone(), 5));
        assert!(notifications_fit_queue(0, 0, 1, &one_page));
        assert!(!notifications_fit_queue(
            0,
            0,
            1,
            &[one_page, vec![smallest]].concat()
        ));

        let entry_that_requires_the_next_page = pending(1, 30);
        assert!(!notifications_fit_queue(
            8_160,
            0,
            1,
            &[entry_that_requires_the_next_page]
        ));
    }

    #[test]
    fn queue_warning_identifies_the_oldest_transaction_and_is_throttled() {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let wake = Arc::new(Condvar::new());
        let mut state = NotificationHubState {
            head_position: NOTIFICATION_QUEUE_PAGE_BYTES,
            ..NotificationHubState::default()
        };
        state.listeners.insert(
            7,
            NotificationListener {
                process_id: 42,
                channels: vec!["events".into()],
                queue: Arc::downgrade(&queue),
                wake: Arc::downgrade(&wake),
                next_sequence: 0,
                position: 0,
                transaction_open: true,
                lease: None,
            },
        );
        let hub = NotificationHub {
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: 1,
            cross: None,
            cross_error: Mutex::new(None),
        };
        assert_eq!(
            hub.queue_warning(&mut state).as_deref(),
            Some("NOTIFY queue is 100% full\nDETAIL: The server process with PID 42 is among those with the oldest transactions.\nHINT: The NOTIFY queue cannot be emptied until that process ends its current transaction.")
        );
        assert!(hub.queue_warning(&mut state).is_none());
    }
}
