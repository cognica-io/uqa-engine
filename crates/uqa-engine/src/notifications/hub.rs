//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database-scoped notification hub state transitions and cross-process synchronization.

mod delivery;
mod subscriptions;
#[cfg(all(test, any(windows, all(unix, not(target_os = "emscripten")))))]
mod tests;

use uqa_storage::notifications::{
    NotificationListenerKey, NotificationListenerSummary, NotificationWakePorts,
};

use super::{
    append_notification, notifications_fit_queue, projected_tail_position, queue_page, queue_usage,
    Arc, Condvar, CrossNotificationCommit, CrossNotificationRequest, CrossProcessCoordinator,
    CrossProcessListenerRow, CrossProcessQueueState, CrossProcessRegistryTransaction, Instant,
    ListenerLease, Mutex, MutexGuard, NotificationHub, NotificationHubState, NotificationListener,
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
    pub(super) fn for_database_file(
        path: &std::path::Path,
        encryption_key: Option<uqa_storage::StorageEncryptionKey>,
    ) -> Arc<Self> {
        let database_path = path.to_path_buf();
        Arc::new_cyclic(|hub| Self {
            admissions: super::registration::SubscriptionAdmissions::default(),
            commit_gate: Mutex::new(()),
            state: Mutex::new(NotificationHubState::default()),
            max_queue_pages: MAX_NOTIFICATION_QUEUE_PAGES,
            cross: Some(CrossProcessState {
                database_path,
                encryption_key,
                registry: Mutex::new(None),
                hub: hub.clone(),
                coordinator: Mutex::new(None),
            }),
            cross_error: Mutex::new(None),
        })
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    pub(super) fn for_database_file(
        _path: &std::path::Path,
        _encryption_key: Option<uqa_storage::StorageEncryptionKey>,
    ) -> Arc<Self> {
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
    ) -> Result<NotificationListenerSummary, SQLError> {
        let local_owner_ids = Self::local_owner_ids(state);
        let mut live = NotificationListenerSummary::default();
        let mut after = None;
        while let Some(listener) = registry.listener_metadata_after(after)? {
            after = Some(listener.key);
            if additional_local_owner == Some(listener.key.owner_id)
                || cross.listener_is_alive(listener.key.owner_id, &local_owner_ids)?
            {
                live.include(listener, !local_owner_ids.contains(&listener.key.owner_id))
                    .map_err(|error| {
                        uqa_execution::storage_errors::storage_error(
                            "asynchronous notification listener summary",
                            &error,
                        )
                    })?;
            } else {
                registry.drop_listener(listener.key.owner_id, listener.key.session_id)?;
            }
        }
        Ok(live)
    }

    fn load_cross_listener(
        registry: &CrossProcessRegistryTransaction,
        state: &NotificationHubState,
        owner_id: [u8; 16],
        session_id: u64,
    ) -> Result<CrossProcessListenerRow, SQLError> {
        let max_channels = state
            .listeners
            .get(&session_id)
            .filter(|listener| listener.subscription.is_some())
            .map(|listener| listener.channels.len());
        registry
            .listener(
                NotificationListenerKey {
                    owner_id,
                    session_id,
                },
                max_channels,
            )?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "committed asynchronous notification listener {session_id} is missing"
                ))
            })
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
        listeners: &NotificationListenerSummary,
        next_sequence: u64,
    ) -> Result<(), SQLError> {
        registry.delete_entries_before(listeners.next_sequence.unwrap_or(next_sequence))
    }

    fn prepare_cross_sync(
        cross: &CrossProcessCoordinator,
        registry: &CrossProcessRegistryTransaction,
        state: &mut NotificationHubState,
        transaction_state: Option<(u64, bool)>,
    ) -> Result<Vec<PreparedDelivery>, SQLError> {
        Self::remove_dead_listeners(state);
        let queue_state = Self::load_cross_queue_state(registry)?;
        let transition = transaction_state.and_then(|(session_id, transaction_open)| {
            state
                .listeners
                .get(&session_id)
                .and_then(|listener| listener.lease.as_ref())
                .map(|lease| (session_id, lease.owner_id(), transaction_open))
        });
        if let Some((session_id, owner_id, false)) = transition {
            let mut listener = Self::load_cross_listener(registry, state, owner_id, session_id)?;
            if listener.transaction_open {
                listener.transaction_open = false;
                Self::save_cross_listener(registry, &listener)?;
            }
        }
        let deliveries =
            Self::cross_deliveries(registry, queue_state, state, &cross.recovery_control()?)?;
        if let Some((session_id, owner_id, true)) = transition {
            let mut listener = Self::load_cross_listener(registry, state, owner_id, session_id)?;
            listener.transaction_open = true;
            Self::save_cross_listener(registry, &listener)?;
        }
        // Fold after delivery has persisted cursors or removed closed listeners in this transaction.
        let listeners = Self::live_cross_listeners(cross, registry, state, None)?;
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
        let Some(cross) = cross_state.initialized_coordinator() else {
            return Ok(());
        };
        if !cross.recovery_initialized() {
            return Ok(());
        }
        let control = cross.recovery_control()?;
        let cancellation = Some(control.cancellation());
        if transaction_state.is_none() {
            let owners =
                Self::local_owner_ids(&*super::registration::lock(&self.state, cancellation)?);
            if !cross.poll_needed(&owners)? {
                *super::registration::lock(&self.cross_error, cancellation)? = None;
                return Ok(());
            }
        }
        let transaction = cross.begin_registry_transaction()?;
        let _gate = super::registration::lock(&self.commit_gate, cancellation)?;
        let mut state = super::registration::lock(&self.state, cancellation)?;
        let deliveries =
            Self::prepare_cross_sync(&cross, &transaction, &mut state, transaction_state)?;
        Self::complete_deliveries(
            transaction.commit_with_control(&control),
            &mut state,
            deliveries,
        )?;
        if let Some((session_id, transaction_open)) = transaction_state {
            if let Some(listener) = state.listeners.get_mut(&session_id) {
                listener.transaction_open = transaction_open;
            }
        }
        if let Ok(mut error) = super::registration::lock(&self.cross_error, cancellation) {
            *error = None;
        }
        Ok(())
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn synchronize_cross_process_notifications(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) {
        if let Err(error) = self.try_synchronize_cross_process_notifications() {
            self.record_cross_error(error, cancellation);
        }
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn record_cross_error(
        &self,
        error: SQLError,
        cancellation: &uqa_core::CancellationToken,
    ) {
        let Ok(mut recorded) = super::registration::lock(&self.cross_error, Some(cancellation))
        else {
            return;
        };
        *recorded = Some(error.to_string());
        drop(recorded);
        let failure = super::subscription::NotificationSubscriptionError::with_source(
            uqa_core::notifications::NotificationFailureKind::SourceUnavailable,
            error,
        );
        let Ok(state) = super::registration::lock(&self.state, Some(cancellation)) else {
            return;
        };
        for listener in state.listeners.values() {
            if let Some(inbox) = listener
                .subscription
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
            {
                inbox.fail(failure.clone());
            }
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
            Self::complete_deliveries(transaction.commit(), &mut state, deliveries)?;
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
        let mut subscription = None;
        if final_channels.is_empty() {
            if let Some(owner_id) = existing_owner {
                let mut listener =
                    Self::load_cross_listener(registry, state, owner_id, session_id)?;
                listener.channels.clear();
                listener.transaction_open = false;
                subscription = Some(listener);
                registry.drop_listener(owner_id, session_id)?;
            }
        } else {
            let owner_id = owner_id.expect("nonempty channels have a listener lease");
            let mut listener = if existing_owner.is_some() {
                Self::load_cross_listener(registry, state, owner_id, session_id)?
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
            subscription = Some(listener);
        }
        let listeners = Self::live_cross_listeners(
            cross,
            registry,
            state,
            owner_id.filter(|_| existing_owner.is_none()),
        )?;
        Ok(PreparedCrossSubscription {
            new_lease,
            listeners,
            subscription,
        })
    }

    pub(super) fn prepare_cross_commit(
        &self,
        cross: &CrossProcessCoordinator,
        mut registry: CrossProcessRegistryTransaction,
        request: CrossNotificationRequest<'_>,
    ) -> Result<CrossNotificationCommit, SQLError> {
        let CrossNotificationRequest {
            session_id,
            process_id,
            channels: final_channels,
            pending,
            control,
            durable_publication,
        } = request;
        let state = self.state.lock();
        let previous_publication = registry.pending_acknowledgement();
        let queue_state = Self::load_cross_queue_state(&registry)?;
        let PreparedCrossSubscription {
            new_lease,
            listeners,
            subscription,
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
            .oldest
            .map_or(queue_state.head_position, |listener| listener.position);
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

        let pending = if listeners.oldest.is_none() {
            &[]
        } else {
            pending
        };
        let publication = if durable_publication {
            Some(registry.prepare_publication(
                process_id,
                pending,
                subscription.as_ref(),
                control,
            )?)
        } else {
            None
        };
        let queue_state = Self::load_cross_queue_state(&registry)?;
        let warning = self.cross_queue_warning(&state, &listeners, queue_state);
        let wake_ports = if pending.is_empty() {
            NotificationWakePorts::default()
        } else {
            listeners.wake_ports
        };
        let publisher_lease = publication
            .as_ref()
            .map(|_| cross.create_listener_lease())
            .transpose()?;
        Ok(CrossNotificationCommit {
            registry: Some(registry),
            new_lease,
            publisher_lease,
            publication_applied: false,
            publication,
            previous_publication,
            wake_ports,
            warning,
        })
    }

    fn cross_queue_warning(
        &self,
        state: &NotificationHubState,
        listeners: &NotificationListenerSummary,
        queue_state: CrossProcessQueueState,
    ) -> Option<uqa_sql::SQLNotice> {
        let tail = listeners
            .oldest
            .map_or(queue_state.head_position, |listener| listener.position);
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
        let blocker = listeners.oldest?.process_id;
        Some(queue_fill_warning(usage, blocker))
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
                    subscription: None,
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
        if let Some(warning) = self.queue_warning(&mut state) {
            notices.lock().push(warning);
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
            Self::complete_deliveries(transaction.commit(), &mut state, deliveries)?;
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
        notices: &Arc<Mutex<Vec<uqa_sql::SQLNotice>>>,
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
                    subscription: None,
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
        notices: &Arc<Mutex<Vec<uqa_sql::SQLNotice>>>,
    ) -> Result<(), SQLError> {
        let cross_state = self.cross.as_ref().ok_or_else(|| {
            SQLError::Internal("cross-process notification coordinator is missing".into())
        })?;
        let cross = cross_state.coordinator()?;
        let transaction = cross.begin_registry_transaction()?;
        let gate = self.commit_gate.lock();
        let prepared = self.prepare_cross_commit(
            &cross,
            transaction,
            CrossNotificationRequest {
                session_id,
                process_id,
                channels: &channels,
                pending: &[],
                control: &cross.recovery_control()?,
                durable_publication: false,
            },
        )?;
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
            false,
        )
    }

    pub(super) fn finalize_cross_commit(
        &self,
        gate: MutexGuard<'_, ()>,
        mut prepared: CrossNotificationCommit,
        session: NotificationSessionCommit<'_>,
        data_committed: bool,
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
        let publication_result = prepared
            .registry
            .take()
            .expect("prepared cross-process notification commit has a registry transaction")
            .commit();
        if !data_committed {
            publication_result
                .as_ref()
                .map_err(|error| SQLError::Internal(error.to_string()))?;
        }
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
                    subscription: None,
                    next_sequence: 0,
                    position: 0,
                    transaction_open: false,
                    lease: prepared.new_lease.take(),
                },
            );
        }
        if let Some(warning) = prepared.warning {
            state.last_queue_warning = Some(Instant::now());
            notices.lock().push(warning);
        }
        drop(state);
        drop(gate);
        CrossProcessCoordinator::wake(wake_ports.iter());
        publication_result.map_err(|error| {
            SQLError::Internal(format!(
                "transaction committed; notification publication awaits recovery: {error}"
            ))
        })?;
        self.try_synchronize_cross_process_session(None)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "transaction committed; notification delivery awaits recovery: {error}"
                ))
            })
    }

    fn remove_dead_listeners(state: &mut NotificationHubState) {
        state.listeners.retain(|_, listener| {
            listener.subscription.as_ref().map_or_else(
                || listener.queue.strong_count() != 0,
                |inbox| inbox.upgrade().is_some_and(|inbox| !inbox.is_closed()),
            )
        });
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

    pub(super) fn queue_warning(
        &self,
        state: &mut NotificationHubState,
    ) -> Option<uqa_sql::SQLNotice> {
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
        Some(queue_fill_warning(
            queue_usage(state, self.max_queue_pages),
            blocker,
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
                .oldest
                .map_or(queue_state.head_position, |listener| listener.position);
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

/// `asyncQueueFillWarning`: how full the queue is, and the listener whose position holds it.
fn queue_fill_warning(usage: f64, blocker: i32) -> uqa_sql::SQLNotice {
    uqa_sql::SQLNotice::warning(format!("NOTIFY queue is {:.0}% full", usage * 100.0))
        .with_detail(Some(format!(
            "The server process with PID {blocker} is among those with the oldest transactions."
        )))
        .with_hint(Some(
            "The NOTIFY queue cannot be emptied until that process ends its current transaction."
                .into(),
        ))
}
