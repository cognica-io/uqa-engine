//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database-scoped notification hub state transitions and cross-process synchronization.

use super::{
    append_notification, notification_end_position, notifications_fit_queue,
    projected_tail_position, queue_page, queue_usage, Arc, Condvar, CrossNotificationCommit,
    CrossProcessCoordinator, CrossProcessListenerRow, CrossProcessQueueEntry,
    CrossProcessQueueState, CrossProcessRegistryTransaction, Instant, ListenerLease, Mutex,
    MutexGuard, NotificationHub, NotificationHubState, NotificationListener,
    NotificationSessionCommit, PendingNotification, PreparedCrossSubscription, PreparedDelivery,
    SQLError, SQLNotification, VecDeque, NOTIFICATION_QUEUE_WARNING_INTERVAL,
};
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use super::{CrossProcessState, MAX_NOTIFICATION_QUEUE_PAGES};

impl NotificationHub {
    pub(crate) fn allocate_backend_process_id(&self) -> Result<Option<i32>, SQLError> {
        let Some(cross_state) = self.cross.as_ref() else {
            return Ok(None);
        };
        cross_state.allocate_backend_process_id().map(Some)
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn for_database_file(path: &std::path::Path) -> Arc<Self> {
        let database_path = path.to_path_buf();
        Arc::new_cyclic(|hub| Self {
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: MAX_NOTIFICATION_QUEUE_PAGES,
            cross: Some(CrossProcessState {
                database_path,
                hub: hub.clone(),
                coordinator: Mutex::new(None),
            }),
            cross_error: Mutex::new(None),
        })
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    pub(super) fn for_database_file(_path: &std::path::Path) -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn local_owner_ids(state: &NotificationHubState) -> Vec<[u8; 16]> {
        state
            .listeners
            .values()
            .filter_map(|listener| listener.lease.as_ref().map(ListenerLease::owner_id))
            .collect()
    }

    fn live_cross_listeners(
        cross: &CrossProcessCoordinator,
        registry: &CrossProcessRegistryTransaction,
        state: &NotificationHubState,
        additional_local_owner: Option<[u8; 16]>,
    ) -> Result<Vec<CrossProcessListenerRow>, SQLError> {
        let mut local_owner_ids = Self::local_owner_ids(state);
        if let Some(owner_id) = additional_local_owner {
            local_owner_ids.push(owner_id);
        }
        let mut live = Vec::new();
        for listener in registry.listeners()? {
            if cross.listener_is_alive(listener.owner_id, &local_owner_ids)? {
                live.push(listener);
            } else {
                registry.drop_listener(listener.owner_id, listener.session_id)?;
            }
        }
        Ok(live)
    }

    fn load_cross_queue_state(
        registry: &CrossProcessRegistryTransaction,
    ) -> Result<CrossProcessQueueState, SQLError> {
        registry.queue_state()
    }

    fn save_cross_listener(
        registry: &CrossProcessRegistryTransaction,
        listener: &CrossProcessListenerRow,
    ) -> Result<(), SQLError> {
        registry.save_listener(listener)
    }

    fn cleanup_cross_entries(
        registry: &CrossProcessRegistryTransaction,
        listeners: &[CrossProcessListenerRow],
        next_sequence: u64,
    ) -> Result<(), SQLError> {
        let tail_sequence = listeners
            .iter()
            .map(|listener| listener.next_sequence)
            .min()
            .unwrap_or(next_sequence);
        registry.delete_entries_before(tail_sequence)
    }

    fn cross_deliveries(
        registry: &CrossProcessRegistryTransaction,
        listeners: &mut [CrossProcessListenerRow],
        local_sessions: &[(u64, [u8; 16])],
        queue_state: CrossProcessQueueState,
    ) -> Result<Vec<PreparedDelivery>, SQLError> {
        let mut deliveries = Vec::new();
        for (session_id, owner_id) in local_sessions {
            let Some(listener) = listeners.iter_mut().find(|listener| {
                listener.owner_id == *owner_id && listener.session_id == *session_id
            }) else {
                continue;
            };
            if listener.transaction_open {
                continue;
            }
            let entries = registry.entries_from(listener.next_sequence)?;
            let notifications = entries
                .into_iter()
                .filter(|entry| listener.channels.contains(&entry.channel))
                .map(|entry| SQLNotification {
                    process_id: entry.process_id,
                    channel: entry.channel,
                    payload: entry.payload,
                })
                .collect::<Vec<_>>();
            listener.next_sequence = queue_state.next_sequence;
            listener.position = queue_state.head_position;
            Self::save_cross_listener(registry, listener)?;
            if !notifications.is_empty() {
                deliveries.push(PreparedDelivery {
                    session_id: *session_id,
                    notifications,
                });
            }
        }
        Ok(deliveries)
    }

    fn apply_deliveries(state: &NotificationHubState, deliveries: Vec<PreparedDelivery>) {
        for delivery in deliveries {
            let Some(listener) = state.listeners.get(&delivery.session_id) else {
                continue;
            };
            let Some(queue) = listener.queue.upgrade() else {
                continue;
            };
            queue.lock().extend(delivery.notifications);
            if let Some(wake) = listener.wake.upgrade() {
                wake.notify_all();
            }
        }
    }

    fn prepare_cross_sync(
        cross: &CrossProcessCoordinator,
        registry: &CrossProcessRegistryTransaction,
        state: &mut NotificationHubState,
        transaction_state: Option<(u64, bool)>,
    ) -> Result<Vec<PreparedDelivery>, SQLError> {
        let mut listeners = Self::live_cross_listeners(cross, registry, state, None)?;
        let queue_state = Self::load_cross_queue_state(registry)?;
        if let Some((session_id, transaction_open)) = transaction_state {
            let owner_id = state
                .listeners
                .get(&session_id)
                .and_then(|listener| listener.lease.as_ref())
                .map(ListenerLease::owner_id);
            if let Some(owner_id) = owner_id {
                let listener = listeners
                    .iter_mut()
                    .find(|listener| {
                        listener.owner_id == owner_id && listener.session_id == session_id
                    })
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "committed asynchronous notification listener {session_id} is missing"
                        ))
                    })?;
                if !transaction_open {
                    listener.transaction_open = false;
                }
            }
        }
        let local_sessions = state
            .listeners
            .iter()
            .filter_map(|(session_id, listener)| {
                listener
                    .lease
                    .as_ref()
                    .map(|lease| (*session_id, lease.owner_id()))
            })
            .collect::<Vec<_>>();
        let deliveries =
            Self::cross_deliveries(registry, &mut listeners, &local_sessions, queue_state)?;
        if let Some((session_id, transaction_open)) = transaction_state {
            if transaction_open {
                let owner_id = state
                    .listeners
                    .get(&session_id)
                    .and_then(|listener| listener.lease.as_ref())
                    .map(ListenerLease::owner_id);
                if let Some(owner_id) = owner_id {
                    let listener = listeners
                        .iter_mut()
                        .find(|listener| {
                            listener.owner_id == owner_id && listener.session_id == session_id
                        })
                        .ok_or_else(|| {
                            SQLError::Internal(format!(
                                "committed asynchronous notification listener {session_id} is missing"
                            ))
                        })?;
                    listener.transaction_open = true;
                    Self::save_cross_listener(registry, listener)?;
                }
            }
        }
        Self::cleanup_cross_entries(registry, &listeners, queue_state.next_sequence)?;
        Ok(deliveries)
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    fn try_synchronize_cross_process_notifications(&self) -> Result<(), SQLError> {
        self.try_synchronize_cross_process_session(None)
    }

    pub(super) fn try_synchronize_cross_process_session(
        &self,
        transaction_state: Option<(u64, bool)>,
    ) -> Result<(), SQLError> {
        let Some(cross_state) = self.cross.as_ref() else {
            return Ok(());
        };
        if self.state.lock().listeners.is_empty() {
            return Ok(());
        }
        let cross = cross_state.coordinator()?;
        let transaction = cross.begin_registry_transaction()?;
        let _gate = self.commit_gate.lock();
        let mut state = self.state.lock();
        let deliveries =
            Self::prepare_cross_sync(&cross, &transaction, &mut state, transaction_state)?;
        transaction.commit()?;
        Self::apply_deliveries(&state, deliveries);
        if let Some((session_id, transaction_open)) = transaction_state {
            if let Some(listener) = state.listeners.get_mut(&session_id) {
                listener.transaction_open = transaction_open;
            }
        }
        *self.cross_error.lock() = None;
        Ok(())
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn synchronize_cross_process_notifications(&self) {
        if let Err(error) = self.try_synchronize_cross_process_notifications() {
            self.record_cross_error(error.to_string());
        }
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn record_cross_error(&self, error: String) {
        *self.cross_error.lock() = Some(error);
        for listener in self.state.lock().listeners.values() {
            if let Some(wake) = listener.wake.upgrade() {
                wake.notify_all();
            }
        }
    }

    pub(super) fn begin_transaction(&self, session_id: u64) -> Result<(), SQLError> {
        if let Some(cross_state) = self.cross.as_ref() {
            if !self.state.lock().listeners.contains_key(&session_id) {
                return Ok(());
            }
            let cross = cross_state.coordinator()?;
            let transaction = cross.begin_registry_transaction()?;
            let _gate = self.commit_gate.lock();
            let mut state = self.state.lock();
            let deliveries = Self::prepare_cross_sync(
                &cross,
                &transaction,
                &mut state,
                Some((session_id, true)),
            )?;
            transaction.commit()?;
            Self::apply_deliveries(&state, deliveries);
            if let Some(listener) = state.listeners.get_mut(&session_id) {
                listener.transaction_open = true;
            }
            return Ok(());
        }
        let _commit = self.commit_gate.lock();
        if let Some(listener) = self.state.lock().listeners.get_mut(&session_id) {
            listener.transaction_open = true;
        }
        Ok(())
    }

    fn prepare_cross_subscription(
        cross: &CrossProcessCoordinator,
        registry: &CrossProcessRegistryTransaction,
        state: &NotificationHubState,
        queue_state: CrossProcessQueueState,
        session_id: u64,
        process_id: i32,
        final_channels: &[String],
    ) -> Result<PreparedCrossSubscription, SQLError> {
        let existing_owner = state
            .listeners
            .get(&session_id)
            .and_then(|listener| listener.lease.as_ref())
            .map(ListenerLease::owner_id);
        let new_lease = if !final_channels.is_empty() && existing_owner.is_none() {
            Some(cross.create_listener_lease()?)
        } else {
            None
        };
        let owner_id = existing_owner.or_else(|| new_lease.as_ref().map(ListenerLease::owner_id));
        let mut listeners = Self::live_cross_listeners(
            cross,
            registry,
            state,
            owner_id.filter(|_| existing_owner.is_none()),
        )?;
        if final_channels.is_empty() {
            if let Some(owner_id) = existing_owner {
                registry.drop_listener(owner_id, session_id)?;
                listeners.retain(|listener| {
                    listener.owner_id != owner_id || listener.session_id != session_id
                });
            }
        } else {
            let owner_id = owner_id.expect("nonempty channels have a listener lease");
            let mut listener = if let Some(existing) = listeners
                .iter()
                .find(|listener| listener.owner_id == owner_id && listener.session_id == session_id)
                .cloned()
            {
                existing
            } else if existing_owner.is_some() {
                return Err(SQLError::Internal(format!(
                    "committed asynchronous notification listener {session_id} is missing"
                )));
            } else {
                CrossProcessListenerRow {
                    owner_id,
                    session_id,
                    process_id,
                    wake_port: cross.wake_port(),
                    channels: Vec::new(),
                    transaction_open: false,
                    next_sequence: queue_state.next_sequence,
                    position: queue_state.head_position,
                }
            };
            listener.process_id = process_id;
            listener.wake_port = cross.wake_port();
            listener.channels = final_channels.to_vec();
            listener.transaction_open = false;
            Self::save_cross_listener(registry, &listener)?;
            if let Some(existing) = listeners.iter_mut().find(|candidate| {
                candidate.owner_id == owner_id && candidate.session_id == session_id
            }) {
                *existing = listener;
            } else {
                listeners.push(listener);
            }
        }
        Ok(PreparedCrossSubscription {
            new_lease,
            owner_id,
            listeners,
        })
    }

    fn append_cross_notifications(
        registry: &CrossProcessRegistryTransaction,
        queue_state: &mut CrossProcessQueueState,
        process_id: i32,
        pending: &[PendingNotification],
    ) -> Result<(), SQLError> {
        let mut entries = Vec::with_capacity(pending.len());
        for notification in pending {
            let end_position = notification_end_position(queue_state.head_position, notification);
            entries.push(CrossProcessQueueEntry {
                sequence: queue_state.next_sequence,
                process_id,
                channel: notification.channel.clone(),
                payload: notification.payload.clone(),
            });
            queue_state.next_sequence =
                queue_state.next_sequence.checked_add(1).ok_or_else(|| {
                    SQLError::Internal("asynchronous notification queue sequence exhausted".into())
                })?;
            queue_state.head_position = end_position;
        }
        registry.append_entries(&entries)?;
        registry.save_queue_state(*queue_state)
    }

    pub(super) fn prepare_cross_commit(
        &self,
        cross: &CrossProcessCoordinator,
        registry: CrossProcessRegistryTransaction,
        session_id: u64,
        process_id: i32,
        final_channels: &[String],
        pending: &[PendingNotification],
    ) -> Result<CrossNotificationCommit, SQLError> {
        let state = self.state.lock();
        let mut queue_state = Self::load_cross_queue_state(&registry)?;
        let PreparedCrossSubscription {
            new_lease,
            owner_id,
            mut listeners,
        } = Self::prepare_cross_subscription(
            cross,
            &registry,
            &state,
            queue_state,
            session_id,
            process_id,
            final_channels,
        )?;

        Self::cleanup_cross_entries(&registry, &listeners, queue_state.next_sequence)?;
        let tail = listeners
            .iter()
            .map(|listener| listener.position)
            .min()
            .unwrap_or(queue_state.head_position);
        if !notifications_fit_queue(
            queue_state.head_position,
            tail,
            self.max_queue_pages,
            pending,
        ) {
            return Err(SQLError::Routine {
                sqlstate: "54000".into(),
                message: "too many notifications in the NOTIFY queue".into(),
            });
        }

        if !listeners.is_empty() && !pending.is_empty() {
            Self::append_cross_notifications(&registry, &mut queue_state, process_id, pending)?;
        }

        let mut local_sessions = state
            .listeners
            .iter()
            .filter_map(|(local_session_id, listener)| {
                (*local_session_id != session_id)
                    .then(|| {
                        listener
                            .lease
                            .as_ref()
                            .map(|lease| (*local_session_id, lease.owner_id()))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        if let Some(owner_id) = owner_id.filter(|_| !final_channels.is_empty()) {
            local_sessions.push((session_id, owner_id));
        }
        let deliveries =
            Self::cross_deliveries(&registry, &mut listeners, &local_sessions, queue_state)?;
        Self::cleanup_cross_entries(&registry, &listeners, queue_state.next_sequence)?;

        let local_owner_ids = local_sessions
            .iter()
            .map(|(_, owner_id)| *owner_id)
            .collect::<Vec<_>>();
        let mut wake_ports = if pending.is_empty() {
            Vec::new()
        } else {
            listeners
                .iter()
                .filter(|listener| !local_owner_ids.contains(&listener.owner_id))
                .map(|listener| listener.wake_port)
                .collect::<Vec<_>>()
        };
        wake_ports.sort_unstable();
        wake_ports.dedup();

        let warning = self.cross_queue_warning(&state, &listeners, queue_state);
        Ok(CrossNotificationCommit {
            registry: Some(registry),
            new_lease,
            deliveries,
            wake_ports,
            warning,
        })
    }

    fn cross_queue_warning(
        &self,
        state: &NotificationHubState,
        listeners: &[CrossProcessListenerRow],
        queue_state: CrossProcessQueueState,
    ) -> Option<String> {
        let tail = listeners
            .iter()
            .map(|listener| listener.position)
            .min()
            .unwrap_or(queue_state.head_position);
        let pages = queue_page(queue_state.head_position).saturating_sub(queue_page(tail));
        let usage = pages as f64 / self.max_queue_pages as f64;
        if usage < 0.5 {
            return None;
        }
        let now = Instant::now();
        if state.last_queue_warning.is_some_and(|last| {
            now.saturating_duration_since(last) < NOTIFICATION_QUEUE_WARNING_INTERVAL
        }) {
            return None;
        }
        let blocker = listeners
            .iter()
            .min_by_key(|listener| listener.position)?
            .process_id;
        Some(format!(
            "NOTIFY queue is {:.0}% full\nDETAIL: The server process with PID {blocker} is among those with the oldest transactions.\nHINT: The NOTIFY queue cannot be emptied until that process ends its current transaction.",
            usage * 100.0
        ))
    }

    pub(super) fn validate_commit(
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

    pub(super) fn commit_session(
        &self,
        _commit: &MutexGuard<'_, ()>,
        session: NotificationSessionCommit<'_>,
    ) {
        let NotificationSessionCommit {
            session_id,
            process_id,
            channels,
            queue,
            wake,
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
            listener.wake = Arc::downgrade(wake);
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
                    wake: Arc::downgrade(wake),
                    next_sequence,
                    position,
                    transaction_open: false,
                    lease: None,
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

    pub(super) fn rollback_session(&self, session_id: u64) -> Result<(), SQLError> {
        if let Some(cross_state) = self.cross.as_ref() {
            if !self.state.lock().listeners.contains_key(&session_id) {
                return Ok(());
            }
            let cross = cross_state.coordinator()?;
            let transaction = cross.begin_registry_transaction()?;
            let _gate = self.commit_gate.lock();
            let mut state = self.state.lock();
            let deliveries = Self::prepare_cross_sync(
                &cross,
                &transaction,
                &mut state,
                Some((session_id, false)),
            )?;
            transaction.commit()?;
            Self::apply_deliveries(&state, deliveries);
            if let Some(listener) = state.listeners.get_mut(&session_id) {
                listener.transaction_open = false;
            }
            return Ok(());
        }
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.transaction_open = false;
        }
        Self::deliver_idle_listeners(&mut state);
        Self::remove_consumed_entries(&mut state);
        Ok(())
    }

    pub(super) fn replace_idle_session(
        &self,
        session_id: u64,
        process_id: i32,
        channels: Vec<String>,
        queue: &Arc<Mutex<VecDeque<SQLNotification>>>,
        wake: &Arc<Condvar>,
        notices: &Arc<Mutex<Vec<(String, String)>>>,
    ) -> Result<(), SQLError> {
        if channels.is_empty() && !self.state.lock().listeners.contains_key(&session_id) {
            return Ok(());
        }
        if self.cross.is_some() {
            return self.replace_cross_idle_session(
                session_id, process_id, channels, queue, wake, notices,
            );
        }
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        if channels.is_empty() {
            state.listeners.remove(&session_id);
        } else if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.process_id = process_id;
            listener.channels = channels;
            listener.queue = Arc::downgrade(queue);
            listener.wake = Arc::downgrade(wake);
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
                    wake: Arc::downgrade(wake),
                    next_sequence,
                    position,
                    transaction_open: false,
                    lease: None,
                },
            );
        }
        Self::deliver_idle_listeners(&mut state);
        Self::remove_consumed_entries(&mut state);
        Ok(())
    }

    fn replace_cross_idle_session(
        &self,
        session_id: u64,
        process_id: i32,
        channels: Vec<String>,
        queue: &Arc<Mutex<VecDeque<SQLNotification>>>,
        wake: &Arc<Condvar>,
        notices: &Arc<Mutex<Vec<(String, String)>>>,
    ) -> Result<(), SQLError> {
        let cross_state = self.cross.as_ref().ok_or_else(|| {
            SQLError::Internal("cross-process notification coordinator is missing".into())
        })?;
        let cross = cross_state.coordinator()?;
        let transaction = cross.begin_registry_transaction()?;
        let gate = self.commit_gate.lock();
        let prepared =
            self.prepare_cross_commit(&cross, transaction, session_id, process_id, &channels, &[])?;
        self.finalize_cross_commit(
            gate,
            prepared,
            NotificationSessionCommit {
                session_id,
                process_id,
                channels,
                queue,
                wake,
                notices,
                pending: &[],
            },
        )
    }

    pub(super) fn finalize_cross_commit(
        &self,
        gate: MutexGuard<'_, ()>,
        mut prepared: CrossNotificationCommit,
        session: NotificationSessionCommit<'_>,
    ) -> Result<(), SQLError> {
        let NotificationSessionCommit {
            session_id,
            process_id,
            channels,
            queue,
            wake,
            notices,
            pending: _,
        } = session;
        prepared
            .registry
            .take()
            .expect("prepared cross-process notification commit has a registry transaction")
            .commit()?;
        let wake_ports = std::mem::take(&mut prepared.wake_ports);
        let mut state = self.state.lock();
        if channels.is_empty() {
            state.listeners.remove(&session_id);
        } else if let Some(listener) = state.listeners.get_mut(&session_id) {
            listener.process_id = process_id;
            listener.channels = channels;
            listener.queue = Arc::downgrade(queue);
            listener.wake = Arc::downgrade(wake);
            listener.transaction_open = false;
        } else {
            state.listeners.insert(
                session_id,
                NotificationListener {
                    process_id,
                    channels,
                    queue: Arc::downgrade(queue),
                    wake: Arc::downgrade(wake),
                    next_sequence: 0,
                    position: 0,
                    transaction_open: false,
                    lease: prepared.new_lease.take(),
                },
            );
        }
        Self::apply_deliveries(&state, prepared.deliveries);
        if let Some(message) = prepared.warning {
            state.last_queue_warning = Some(Instant::now());
            notices.lock().push(("WARNING".into(), message));
        }
        drop(state);
        drop(gate);
        CrossProcessCoordinator::wake(&wake_ports);
        Ok(())
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
            if let Some(wake) = listener.wake.upgrade() {
                wake.notify_all();
            }
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

    pub(super) fn queue_warning(&self, state: &mut NotificationHubState) -> Option<String> {
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

    pub(super) fn usage(&self) -> Result<f64, SQLError> {
        if let Some(cross_state) = self.cross.as_ref() {
            let cross = cross_state.coordinator()?;
            let transaction = cross.begin_registry_transaction()?;
            let _gate = self.commit_gate.lock();
            let state = self.state.lock();
            let listeners = Self::live_cross_listeners(&cross, &transaction, &state, None)?;
            let queue_state = Self::load_cross_queue_state(&transaction)?;
            Self::cleanup_cross_entries(&transaction, &listeners, queue_state.next_sequence)?;
            let tail = listeners
                .iter()
                .map(|listener| listener.position)
                .min()
                .unwrap_or(queue_state.head_position);
            let pages = queue_page(queue_state.head_position).saturating_sub(queue_page(tail));
            let usage = pages as f64 / self.max_queue_pages as f64;
            transaction.commit()?;
            return Ok(usage);
        }
        let mut state = self.state.lock();
        Self::remove_dead_listeners(&mut state);
        Ok(queue_usage(&state, self.max_queue_pages))
    }

    pub(crate) fn unregister(&self, session_id: u64) {
        if let Some(cross_state) = self.cross.as_ref() {
            if !self.state.lock().listeners.contains_key(&session_id) {
                return;
            }
            let outcome = (|| {
                let cross = cross_state.coordinator()?;
                let transaction = cross.begin_registry_transaction()?;
                let _gate = self.commit_gate.lock();
                let mut state = self.state.lock();
                if let Some(owner_id) = state
                    .listeners
                    .get(&session_id)
                    .and_then(|listener| listener.lease.as_ref())
                    .map(ListenerLease::owner_id)
                {
                    transaction.drop_listener(owner_id, session_id)?;
                }
                state.listeners.remove(&session_id);
                let listeners = Self::live_cross_listeners(&cross, &transaction, &state, None)?;
                let queue_state = Self::load_cross_queue_state(&transaction)?;
                Self::cleanup_cross_entries(&transaction, &listeners, queue_state.next_sequence)?;
                transaction.commit()
            })();
            if let Err(error) = outcome {
                self.state.lock().listeners.remove(&session_id);
                *self.cross_error.lock() = Some(error.to_string());
            }
            return;
        }
        let _commit = self.commit_gate.lock();
        let mut state = self.state.lock();
        state.listeners.remove(&session_id);
        Self::remove_consumed_entries(&mut state);
    }
}
