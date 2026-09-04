//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional SQL asynchronous-notification coordination.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::{Mutex, MutexGuard};
use uqa_sql::SQLError;

use crate::Engine;

const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 8_000;

/// One committed SQL notification waiting for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLNotification {
    /// Engine-session identity of the sender. Wire adapters may map this to their backend process identifier.
    pub sender_session_id: u64,
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

struct NotificationListener {
    channels: BTreeSet<String>,
    queue: Weak<Mutex<VecDeque<SQLNotification>>>,
}

#[derive(Default)]
pub(crate) struct NotificationHub {
    commit_gate: Mutex<()>,
    listeners: Mutex<BTreeMap<u64, NotificationListener>>,
}

impl NotificationHub {
    fn commit_session(
        &self,
        _commit: &MutexGuard<'_, ()>,
        session_id: u64,
        channels: BTreeSet<String>,
        queue: &Arc<Mutex<VecDeque<SQLNotification>>>,
        received_queue_len_at_begin: Option<usize>,
        pending: &[PendingNotification],
    ) {
        let mut listeners = self.listeners.lock();
        if let Some(queue_len_at_begin) = received_queue_len_at_begin {
            let mut queue = queue.lock();
            let mut received_during_transaction = queue.split_off(queue_len_at_begin);
            received_during_transaction
                .retain(|notification| channels.contains(&notification.channel));
            queue.append(&mut received_during_transaction);
        }
        if channels.is_empty() {
            listeners.remove(&session_id);
        } else {
            listeners.insert(
                session_id,
                NotificationListener {
                    channels,
                    queue: Arc::downgrade(queue),
                },
            );
        }
        if pending.is_empty() {
            return;
        }
        let mut recipients = Vec::with_capacity(listeners.len());
        listeners.retain(|_, listener| {
            let Some(queue) = listener.queue.upgrade() else {
                return false;
            };
            recipients.push((listener.channels.clone(), queue));
            true
        });
        for notification in pending {
            for (channels, queue) in &recipients {
                if channels.contains(&notification.channel) {
                    queue.lock().push_back(SQLNotification {
                        sender_session_id: session_id,
                        channel: notification.channel.clone(),
                        payload: notification.payload.clone(),
                    });
                }
            }
        }
    }

    pub(crate) fn unregister(&self, session_id: u64) {
        self.listeners.lock().remove(&session_id);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum NotificationHubIdentity {
    Durable(uqa_storage::PersistentStorageIdentity),
    Provider(usize),
}

static DATABASE_NOTIFICATION_HUBS: OnceLock<
    Mutex<HashMap<NotificationHubIdentity, Weak<NotificationHub>>>,
> = OnceLock::new();

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
    pub(crate) fn listen(&self, channel: &str) {
        self.session
            .state
            .write()
            .listened_channels
            .insert(channel.to_string());
    }

    pub(crate) fn unlisten(&self, channel: Option<&str>) {
        let mut session = self.session.state.write();
        if let Some(channel) = channel {
            session.listened_channels.remove(channel);
        } else {
            session.listened_channels.clear();
        }
    }

    pub(crate) fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError> {
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

    pub(super) fn begin_notification_commit<'a>(
        &'a self,
        outer: bool,
        transaction: &crate::TransactionFrame,
    ) -> Option<MutexGuard<'a, ()>> {
        let channels_changed = transaction.session_snapshot.listened_channels
            != self.session.state.read().listened_channels;
        (outer && (channels_changed || !transaction.pending_notifications.is_empty()))
            .then(|| self.notification_hub.commit_gate.lock())
    }

    pub(super) fn commit_notification_state(
        &self,
        commit: &MutexGuard<'_, ()>,
        transaction: &crate::TransactionFrame,
    ) {
        let channels = self.session.state.read().listened_channels.clone();
        self.notification_hub.commit_session(
            commit,
            self.session_id,
            channels,
            &self.runtime.notifications,
            Some(transaction.notification_queue_len_at_begin),
            &transaction.pending_notifications,
        );
    }

    pub(crate) fn synchronize_notification_listener(&self) {
        let commit = self.notification_hub.commit_gate.lock();
        let channels = self.session.state.read().listened_channels.clone();
        self.notification_hub.commit_session(
            &commit,
            self.session_id,
            channels,
            &self.runtime.notifications,
            None,
            &[],
        );
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
    pub(super) fn merge_pending_notifications(&mut self, pending: Vec<PendingNotification>) {
        for notification in pending {
            if !self.pending_notifications.contains(&notification) {
                self.pending_notifications.push(notification);
            }
        }
    }
}
