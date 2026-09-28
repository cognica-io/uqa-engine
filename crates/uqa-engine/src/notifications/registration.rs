//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded retained registration permits and cancellable Engine state gates.

use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use uqa_core::{notifications::NotificationFailureKind, CancellationToken};
use uqa_sql::SQLError;

use super::subscription::NotificationSubscriptionError;

pub(super) const WAIT_SLICE: Duration = Duration::from_millis(10);

#[cfg(test)]
thread_local! {
    static GATE_WAIT: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn observe_gate_wait(sender: std::sync::mpsc::SyncSender<()>) {
    GATE_WAIT.with(|observer| {
        observer.replace(Some(sender));
    });
}

#[cfg(test)]
pub(crate) fn gate_waited() {
    GATE_WAIT.with(|observer| {
        if let Some(sender) = observer.borrow().as_ref() {
            let _ = sender.try_send(());
        }
    });
}

pub(super) struct SubscriptionAdmission {
    ceiling: usize,
}

#[derive(Default)]
pub(super) struct SubscriptionAdmissions {
    retained: Mutex<Vec<Weak<SubscriptionAdmission>>>,
}

impl SubscriptionAdmissions {
    pub(super) fn reserve(
        &self,
        ceiling: usize,
        cancellation: &CancellationToken,
    ) -> Result<Arc<SubscriptionAdmission>, NotificationSubscriptionError> {
        check(cancellation)?;
        // No operation waits before it owns capacity. This mutex protects only permit metadata, never provider work or another Engine gate.
        let mut retained = self
            .retained
            .try_lock()
            .ok_or_else(|| NotificationSubscriptionError::new(NotificationFailureKind::Capacity))?;
        retained.retain(|permit| permit.strong_count() != 0);
        let effective = retained
            .iter()
            .filter_map(Weak::upgrade)
            .fold(ceiling, |limit, permit| limit.min(permit.ceiling));
        if retained.len() >= effective {
            return Err(NotificationSubscriptionError::new(
                NotificationFailureKind::Capacity,
            ));
        }
        retained.try_reserve_exact(1).map_err(|error| {
            NotificationSubscriptionError::with_source(NotificationFailureKind::Capacity, error)
        })?;
        let permit = Arc::new(SubscriptionAdmission { ceiling });
        retained.push(Arc::downgrade(&permit));
        Ok(permit)
    }
}

pub(super) fn check(cancellation: &CancellationToken) -> Result<(), NotificationSubscriptionError> {
    cancellation.check().map_err(|error| {
        NotificationSubscriptionError::with_source(NotificationFailureKind::Cancelled, error)
    })
}

pub(super) fn lock<'a, T>(
    mutex: &'a Mutex<T>,
    cancellation: Option<&CancellationToken>,
) -> Result<MutexGuard<'a, T>, SQLError> {
    let Some(cancellation) = cancellation else {
        return Ok(mutex.lock());
    };
    loop {
        cancellation.check()?;
        if let Some(guard) = mutex.try_lock_for(WAIT_SLICE) {
            cancellation.check()?;
            return Ok(guard);
        }
        #[cfg(test)]
        gate_waited();
    }
}

pub(super) fn read<'a, T>(
    mutex: &'a RwLock<T>,
    cancellation: &CancellationToken,
) -> Result<RwLockReadGuard<'a, T>, SQLError> {
    loop {
        cancellation.check()?;
        if let Some(guard) = mutex.try_read_for(WAIT_SLICE) {
            cancellation.check()?;
            return Ok(guard);
        }
        #[cfg(test)]
        gate_waited();
    }
}

pub(super) fn failure(error: SQLError) -> NotificationSubscriptionError {
    let kind = match error.sqlstate() {
        Some("57014") => NotificationFailureKind::Cancelled,
        Some("53200") => NotificationFailureKind::Capacity,
        _ => NotificationFailureKind::SourceUnavailable,
    };
    NotificationSubscriptionError::with_source(kind, error)
}

#[cfg(test)]
mod tests;
