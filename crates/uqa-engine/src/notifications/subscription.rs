//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent listener ownership over the original notification hub and provider.

use parking_lot::Mutex;
use std::{
    collections::HashSet,
    error::Error,
    fmt,
    sync::{Arc, Weak},
    task::Poll,
    time::Duration,
};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind, NotificationIdentity};
use uqa_sql::catalog::roles::session::SessionAuthorization;
use uqa_storage::{PersistentStorageBackend, PersistentStorageProvider};

use super::{inbox::SubscriptionInbox, registration, NotificationHubOwner};

/// Explicit limits for an owned listener. No deployment capacity is inferred from SQL `work_mem`.
///
/// Registrations and retained listeners share every admitted handle's active-subscription ceiling. Admission cannot weaken a stricter ceiling retained by another pending registration or live handle. Queue bytes include string capacities, queue slots and transient prepared-delivery buffers; provider caches and the returned application's owned events are separate resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationSubscriptionOptions {
    pub max_active_subscriptions: usize,
    pub max_channels: usize,
    pub max_queued_notifications: usize,
    pub max_queued_bytes: usize,
    pub max_registry_entries_per_poll: usize,
}

impl NotificationSubscriptionOptions {
    pub(super) fn validate(self) -> Result<(), NotificationSubscriptionError> {
        if self.max_active_subscriptions == 0
            || self.max_channels == 0
            || self.max_queued_notifications == 0
            || self.max_queued_bytes == 0
            || self.max_registry_entries_per_poll == 0
        {
            return Err(NotificationSubscriptionError::new(
                NotificationFailureKind::InvalidRequest,
            ));
        }
        Ok(())
    }
}

/// One non-cloneable reservation against the original hub's pending/live limit. Reserve before submitting registration to a runtime queue; dropping an unused permit releases capacity without provider I/O. A permit does not retain the Engine or register any channels.
pub struct NotificationSubscriptionPermit {
    hub: Weak<NotificationHubOwner>,
    admission: Arc<registration::SubscriptionAdmission>,
    options: NotificationSubscriptionOptions,
}

impl fmt::Debug for NotificationSubscriptionPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotificationSubscriptionPermit")
            .finish_non_exhaustive()
    }
}

fn validate_channels(
    channels: &[&str],
    options: NotificationSubscriptionOptions,
    cancellation: &uqa_core::CancellationToken,
) -> Result<(), NotificationSubscriptionError> {
    options.validate()?;
    registration::check(cancellation)?;
    if channels.is_empty() || channels.len() > options.max_channels {
        return Err(NotificationSubscriptionError::new(
            NotificationFailureKind::InvalidRequest,
        ));
    }
    let mut seen = HashSet::new();
    seen.try_reserve(channels.len()).map_err(|error| {
        NotificationSubscriptionError::with_source(NotificationFailureKind::Capacity, error)
    })?;
    for channel in channels {
        registration::check(cancellation)?;
        if channel.contains('\0')
            || super::validate_channel(channel).is_err()
            || !seen.insert(*channel)
        {
            return Err(NotificationSubscriptionError::new(
                NotificationFailureKind::InvalidRequest,
            ));
        }
    }
    registration::check(cancellation)
}

/// A stable failure category with an original cause available only through explicit inspection.
#[derive(Clone)]
pub struct NotificationSubscriptionError {
    kind: NotificationFailureKind,
    original: Option<Arc<dyn Error + Send + Sync>>,
}

impl NotificationSubscriptionError {
    pub(super) fn new(kind: NotificationFailureKind) -> Self {
        Self {
            kind,
            original: None,
        }
    }
    pub(super) fn with_source(
        kind: NotificationFailureKind,
        error: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            original: Some(Arc::new(error)),
        }
    }
    pub fn kind(&self) -> NotificationFailureKind {
        self.kind
    }
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
    /// Inspect the original local failure explicitly; it can contain private provider diagnostics.
    pub fn original_error(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        self.original.as_deref()
    }
}
impl fmt::Debug for NotificationSubscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotificationSubscriptionError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for NotificationSubscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl Error for NotificationSubscriptionError {}

/// A bounded wait ending with one value, elapsed receive time, or explicit closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationWait {
    Event(NotificationEvent),
    TimedOut,
    Closed,
}

pub(super) struct ListenerResources {
    pub(super) hub: Arc<NotificationHubOwner>,
    pub(super) session_id: u64,
    pub(super) locks: Arc<crate::row_locks::RowLockManager>,
    pub(super) _provider: Option<Arc<dyn PersistentStorageProvider>>,
    pub(super) _backend: Option<Arc<dyn PersistentStorageBackend>>,
    pub(super) _admission: Arc<registration::SubscriptionAdmission>,
}

impl Drop for ListenerResources {
    fn drop(&mut self) {
        self.hub.unregister(self.session_id);
        self.locks.release_session(self.session_id);
    }
}

/// An idle listener independent of the query session that created it. Close every retained handle before replacing a database file.
pub struct NotificationSubscription {
    pub(super) inbox: Arc<SubscriptionInbox>,
    pub(super) resources: Mutex<Option<ListenerResources>>,
    pub(super) authorization: SessionAuthorization,
}

impl NotificationSubscription {
    pub fn identity(&self) -> &NotificationIdentity {
        self.inbox.identity()
    }
    /// No database read, SQL execution or blocking receive is performed by this poll.
    pub fn poll(&self) -> Result<Poll<Option<NotificationEvent>>, NotificationSubscriptionError> {
        self.inbox.poll()
    }
    /// Receive without the caller's transaction or SQL timeout. Timeout leaves this listener registered.
    pub fn wait(
        &self,
        timeout: Duration,
    ) -> Result<NotificationWait, NotificationSubscriptionError> {
        self.inbox.wait(timeout)
    }
    /// Receive without blocking a thread or requiring a particular async runtime. At most one asynchronous receive may be pending; another returns `InvalidRequest`. Dropping a pending receive releases its wake slot without consuming an event or closing the listener.
    pub async fn next_event(
        &self,
    ) -> Result<Option<NotificationEvent>, NotificationSubscriptionError> {
        self.inbox.wait_async().await
    }
    pub fn is_closed(&self) -> bool {
        self.inbox.is_closed()
    }
    /// Preserve the exact effective SQL role selected at registration, including its incarnation.
    pub fn role_identity(&self) -> uqa_core::catalog_role::RoleIdentity {
        self.authorization.current().identity()
    }
    /// End delivery and wake receivers without provider I/O. Runtime adapters must still call `close` off their event-loop worker and wait for it to finish; this signal retains registration resources and admission until that cleanup completes.
    pub fn stop_delivery(&self) {
        self.inbox.close();
    }
    /// Stop delivery and wake a blocked receiver before releasing only this listener's retained resources. Concurrent callers wait for that same cleanup to complete.
    pub fn close(&self) {
        self.stop_delivery();
        let mut resources = self.resources.lock();
        drop(resources.take());
    }
}

impl Drop for NotificationSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

impl crate::Engine {
    /// Register one independent idle listener before returning a ready handle. Channels are exact, unique, nonempty UTF-8 names of at most 63 bytes; no identifier folding or truncation is performed. The caller's transaction and low-level SQL listener remain independent.
    ///
    /// Registration and close can perform native provider I/O. Runtime adapters must run them outside their event-loop worker. Polling never performs I/O; a wait timeout does not unsubscribe.
    pub fn subscribe_notifications(
        &self,
        channels: &[&str],
        options: NotificationSubscriptionOptions,
    ) -> Result<NotificationSubscription, NotificationSubscriptionError> {
        self.subscribe_notifications_with_cancellation(
            channels,
            options,
            &uqa_core::CancellationToken::new(),
        )
    }

    /// Register with independent caller cancellation through statement, hub and native registry waits. A pending registration already owns subscription capacity. Cancellation before the committed registration boundary returns `Cancelled` without publishing a partial listener; successful readiness wins a simultaneous cancellation race.
    ///
    /// The signal applies to registration only. After readiness the handle retains its independent lifetime until close/drop. Native file opening and provider-session construction already in progress remain synchronous; this method does not establish an operating-system I/O deadline.
    pub fn subscribe_notifications_with_cancellation(
        &self,
        channels: &[&str],
        options: NotificationSubscriptionOptions,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<NotificationSubscription, NotificationSubscriptionError> {
        validate_channels(channels, options, cancellation)?;
        let permit = self.reserve_notification_subscription(options, cancellation)?;
        self.register_notification_subscription(channels, permit, cancellation)
    }

    /// Reserve the original hub's shared pending/live allowance without waiting on Engine or provider gates. Runtime adapters must retain this Engine separately and obtain the permit before submitting work. The cancellation token applies to this reservation call; the consuming registration accepts its own retained signal.
    pub fn reserve_notification_subscription(
        &self,
        options: NotificationSubscriptionOptions,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<NotificationSubscriptionPermit, NotificationSubscriptionError> {
        options.validate()?;
        let admission = self
            .notification_hub
            .admissions
            .reserve(options.max_active_subscriptions, cancellation)?;
        Ok(NotificationSubscriptionPermit {
            hub: Arc::downgrade(&self.notification_hub),
            admission,
            options,
        })
    }

    /// Consume one pre-submission reservation on its original hub. The permit fixes every limit and cannot be reused or transferred to another database identity. Invalid input, cancellation or failure releases it; readiness transfers that same permit to the listener through completed cleanup.
    pub fn subscribe_notifications_with_permit(
        &self,
        channels: &[&str],
        permit: NotificationSubscriptionPermit,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<NotificationSubscription, NotificationSubscriptionError> {
        if !Weak::ptr_eq(&permit.hub, &Arc::downgrade(&self.notification_hub)) {
            return Err(NotificationSubscriptionError::new(
                NotificationFailureKind::InvalidRequest,
            ));
        }
        validate_channels(channels, permit.options, cancellation)?;
        self.register_notification_subscription(channels, permit, cancellation)
    }

    fn register_notification_subscription(
        &self,
        channels: &[&str],
        permit: NotificationSubscriptionPermit,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<NotificationSubscription, NotificationSubscriptionError> {
        let _statement = self
            .runtime
            .statement_gate
            .lock_with_cancellation(cancellation)
            .map_err(|error| registration::failure(error.into()))?;
        self.prepare_notification_recovery_with_cancellation(Some(cancellation))
            .map_err(registration::failure)?;
        let authorization = registration::read(&self.session.state, cancellation)
            .map_err(registration::failure)?
            .authorization
            .clone();
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| {
            NotificationSubscriptionError::with_source(
                NotificationFailureKind::SourceUnavailable,
                std::io::Error::other(error.to_string()),
            )
        })?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let identity = NotificationIdentity {
            epoch: uqa_core::notifications::NotificationEpoch::from_bytes(bytes)
                .expect("version and variant set above"),
            request_id: None,
        };
        let session_id = self.row_locks.allocate_session();
        let inbox = SubscriptionInbox::new(identity, permit.options);
        self.notification_hub.register_subscription(
            session_id,
            self.backend_process_id(),
            channels
                .iter()
                .map(|channel| (*channel).to_owned())
                .collect(),
            &inbox,
            cancellation,
        )?;
        Ok(NotificationSubscription {
            inbox,
            resources: Mutex::new(Some(ListenerResources {
                hub: self.notification_hub.clone(),
                session_id,
                locks: self.row_locks.clone(),
                _provider: self.storage.provider.clone(),
                _backend: self.storage.backend.clone(),
                _admission: permit.admission,
            })),
            authorization,
        })
    }
}
