//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional SQL asynchronous-notification coordination.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, MutexGuard};
use uqa_sql::SQLError;

use crate::Engine;

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
    next_sequence: u64,
    position: u64,
    transaction_open: bool,
}

struct NotificationSessionCommit<'a> {
    session_id: u64,
    process_id: i32,
    channels: Vec<String>,
    queue: &'a Arc<Mutex<VecDeque<SQLNotification>>>,
    notices: &'a Arc<Mutex<Vec<(String, String)>>>,
    pending: &'a [PendingNotification],
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
}

impl Default for NotificationHub {
    fn default() -> Self {
        Self {
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: MAX_NOTIFICATION_QUEUE_PAGES,
        }
    }
}

impl NotificationHub {
    fn begin_transaction(&self, session_id: u64) {
        let _commit = self.commit_gate.lock();
        if let Some(listener) = self.state.lock().listeners.get_mut(&session_id) {
            listener.transaction_open = true;
        }
    }

    fn validate_commit(
        &self,
        _commit: &MutexGuard<'_, ()>,
        session_id: u64,
        final_channels: &[String],
        pending: &[PendingNotification],
    ) -> Result<(), SQLError> {
        if pending.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock();
        Self::remove_dead_listeners(&mut state);
        let has_recipient = state.listeners.iter().any(|(listener_id, listener)| {
            if *listener_id == session_id {
                !final_channels.is_empty()
            } else {
                !listener.channels.is_empty()
            }
        }) || (!final_channels.is_empty()
            && !state.listeners.contains_key(&session_id));
        if !has_recipient {
            return Ok(());
        }
        let tail = projected_tail_position(&state, session_id, final_channels);
        if !notifications_fit_queue(state.head_position, tail, self.max_queue_pages, pending) {
            return Err(SQLError::Routine {
                sqlstate: "54000".into(),
                message: "too many notifications in the NOTIFY queue".into(),
            });
        }
        Ok(())
    }

    fn commit_session(&self, _commit: &MutexGuard<'_, ()>, session: NotificationSessionCommit<'_>) {
        let NotificationSessionCommit {
            session_id,
            process_id,
            channels,
            queue,
            notices,
            pending,
        } = session;
        let mut state = self.state.lock();
        Self::remove_dead_listeners(&mut state);
        if channels.is_empty() {
            state.listeners.remove(&session_id);
        } else if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.process_id = process_id;
            listener.channels = channels;
            listener.queue = Arc::downgrade(queue);
            listener.transaction_open = false;
        } else {
            let next_sequence = state.next_sequence;
            let position = state.head_position;
            state.listeners.insert(
                session_id,
                NotificationListener {
                    process_id,
                    channels,
                    queue: Arc::downgrade(queue),
                    next_sequence,
                    position,
                    transaction_open: false,
                },
            );
        }
        if !state.listeners.is_empty() {
            for notification in pending {
                append_notification(&mut state, process_id, notification);
            }
        }
        Self::deliver_idle_listeners(&mut state);
        Self::remove_consumed_entries(&mut state);
        if let Some(message) = self.queue_warning(&mut state) {
            notices.lock().push(("WARNING".into(), message));
        }
    }

    fn rollback_session(&self, session_id: u64) {
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.transaction_open = false;
        }
        Self::deliver_idle_listeners(&mut state);
        Self::remove_consumed_entries(&mut state);
    }

    fn replace_idle_session(
        &self,
        session_id: u64,
        process_id: i32,
        channels: Vec<String>,
        queue: &Arc<Mutex<VecDeque<SQLNotification>>>,
    ) {
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        if channels.is_empty() {
            state.listeners.remove(&session_id);
        } else if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.process_id = process_id;
            listener.channels = channels;
            listener.queue = Arc::downgrade(queue);
            listener.transaction_open = false;
        } else {
            let next_sequence = state.next_sequence;
            let position = state.head_position;
            state.listeners.insert(
                session_id,
                NotificationListener {
                    process_id,
                    channels,
                    queue: Arc::downgrade(queue),
                    next_sequence,
                    position,
                    transaction_open: false,
                },
            );
        }
        Self::deliver_idle_listeners(&mut state);
        Self::remove_consumed_entries(&mut state);
    }

    fn deliver_idle_listeners(state: &mut NotificationHubState) {
        let head_position = state.head_position;
        let next_sequence = state.next_sequence;
        let entries = &state.entries;
        let listeners = &mut state.listeners;
        let mut dead = Vec::new();
        for (session_id, listener) in listeners.iter_mut() {
            if listener.transaction_open {
                continue;
            }
            let Some(queue) = listener.queue.upgrade() else {
                dead.push(*session_id);
                continue;
            };
            let delivered = entries
                .iter()
                .filter(|entry| entry.sequence >= listener.next_sequence)
                .filter(|entry| listener.channels.contains(&entry.channel))
                .map(|entry| SQLNotification {
                    process_id: entry.process_id,
                    channel: entry.channel.clone(),
                    payload: entry.payload.clone(),
                })
                .collect::<Vec<_>>();
            queue.lock().extend(delivered);
            listener.next_sequence = next_sequence;
            listener.position = head_position;
        }
        for session_id in dead {
            listeners.remove(&session_id);
        }
    }

    fn remove_dead_listeners(state: &mut NotificationHubState) {
        state
            .listeners
            .retain(|_, listener| listener.queue.strong_count() != 0);
        Self::remove_consumed_entries(state);
    }

    fn remove_consumed_entries(state: &mut NotificationHubState) {
        let tail_sequence = state
            .listeners
            .values()
            .map(|listener| listener.next_sequence)
            .min()
            .unwrap_or(state.next_sequence);
        while state
            .entries
            .front()
            .is_some_and(|entry| entry.sequence < tail_sequence)
        {
            state.entries.pop_front();
        }
    }

    fn queue_warning(&self, state: &mut NotificationHubState) -> Option<String> {
        if queue_usage(state, self.max_queue_pages) < 0.5 {
            return None;
        }
        let now = Instant::now();
        if state.last_queue_warning.is_some_and(|last| {
            now.saturating_duration_since(last) < NOTIFICATION_QUEUE_WARNING_INTERVAL
        }) {
            return None;
        }
        let blocker = state
            .listeners
            .values()
            .min_by_key(|listener| listener.position)?
            .process_id;
        state.last_queue_warning = Some(now);
        let percentage = queue_usage(state, self.max_queue_pages) * 100.0;
        Some(format!(
            "NOTIFY queue is {percentage:.0}% full\nDETAIL: The server process with PID {blocker} is among those with the oldest transactions.\nHINT: The NOTIFY queue cannot be emptied until that process ends its current transaction."
        ))
    }

    fn usage(&self) -> f64 {
        let mut state = self.state.lock();
        Self::remove_dead_listeners(&mut state);
        queue_usage(&state, self.max_queue_pages)
    }

    pub(crate) fn unregister(&self, session_id: u64) {
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        state.listeners.remove(&session_id);
        Self::remove_consumed_entries(&mut state);
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
    let hub = Arc::new(NotificationHub::default());
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

    pub(super) fn begin_notification_transaction(&self) {
        self.notification_hub.begin_transaction(self.session_id);
    }

    pub(super) fn begin_notification_commit<'a>(
        &'a self,
        outer: bool,
        transaction: &crate::TransactionFrame,
    ) -> Result<Option<MutexGuard<'a, ()>>, SQLError> {
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
        let commit = self.notification_hub.commit_gate.lock();
        self.notification_hub.validate_commit(
            &commit,
            self.session_id,
            &final_channels,
            &transaction.pending_notifications,
        )?;
        Ok(Some(commit))
    }

    pub(super) fn commit_notification_state(
        &self,
        commit: &MutexGuard<'_, ()>,
        transaction: &crate::TransactionFrame,
    ) {
        let current_channels = self.session.state.read().listened_channels.clone();
        let channels = transaction.final_listened_channels(&current_channels);
        self.notification_hub.commit_session(
            commit,
            NotificationSessionCommit {
                session_id: self.session_id,
                process_id: self.backend_process_id(),
                channels: channels.clone(),
                queue: &self.runtime.notifications,
                notices: &self.runtime.notices,
                pending: &transaction.pending_notifications,
            },
        );
        self.session.state.write().listened_channels = channels;
    }

    pub(super) fn rollback_notification_state(&self) {
        self.notification_hub.rollback_session(self.session_id);
    }

    pub(crate) fn clear_notification_listener_without_transaction(&self) {
        self.session.state.write().listened_channels.clear();
        self.notification_hub.replace_idle_session(
            self.session_id,
            self.backend_process_id(),
            Vec::new(),
            &self.runtime.notifications,
        );
    }

    /// PostgreSQL-compatible backend process identifier for this logical SQL session.
    #[must_use]
    pub fn backend_process_id(&self) -> i32 {
        self.session.backend_process_id
    }

    pub(crate) fn listening_channels(&self) -> Vec<String> {
        self.session.state.read().listened_channels.clone()
    }

    pub(crate) fn notification_queue_usage(&self) -> f64 {
        self.notification_hub.usage()
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
                next_sequence: 0,
                position: 0,
                transaction_open: true,
            },
        );
        let hub = NotificationHub {
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: 1,
        };
        assert_eq!(
            hub.queue_warning(&mut state).as_deref(),
            Some("NOTIFY queue is 100% full\nDETAIL: The server process with PID 42 is among those with the oldest transactions.\nHINT: The NOTIFY queue cannot be emptied until that process ends its current transaction.")
        );
        assert!(hub.queue_warning(&mut state).is_none());
    }
}
