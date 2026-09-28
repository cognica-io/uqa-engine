//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped native cancellation without replaying evaluated registry statements.

use rusqlite::Connection;
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

use super::{registry_error, REGISTRY_BUSY_TIMEOUT};

struct Context {
    connection: usize,
    cancellation: [Option<uqa_core::CancellationToken>; 2],
    started: Instant,
    interrupted: bool,
}

thread_local! {
    // rusqlite's safe busy callback takes a function pointer rather than a closure. Scope its context to the calling operation, never to the thread that originally created a movable connection lease.
    static ACTIVE: RefCell<Vec<Context>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    static BUSY_OBSERVER: RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn observe_native_wait(sender: std::sync::mpsc::SyncSender<()>) {
    BUSY_OBSERVER.with(|observer| {
        observer.replace(Some(sender));
    });
}

pub(super) struct RegistryOperation<'a> {
    connection: &'a Connection,
    previous_timeout: Duration,
}

pub(super) fn operation<'a>(
    connection: &'a Connection,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<Option<RegistryOperation<'a>>> {
    operation_with(connection, control, None)
}

pub(super) fn operation_with<'a>(
    connection: &'a Connection,
    control: Option<&StorageReadControl>,
    read: Option<&StorageReadControl>,
) -> StorageBackendResult<Option<RegistryOperation<'a>>> {
    if control.is_none()
        && read.is_none()
        && ACTIVE.with(|active| {
            active
                .borrow()
                .last()
                .is_none_or(|context| context.connection == std::ptr::from_ref(connection) as usize)
        })
    {
        return Ok(None);
    }
    for control in [control, read].into_iter().flatten() {
        control.check()?;
    }
    let identity = std::ptr::from_ref(connection) as usize;
    if ACTIVE.with(|active| cancellation_requested(&active.borrow(), identity)) {
        return Err(uqa_core::QueryCancelled.into());
    }
    let milliseconds: u32 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(|error| registry_error("read registry wait policy", &error))?;
    ACTIVE.with(|active| -> StorageBackendResult<()> {
        let mut active = active.borrow_mut();
        active
            .try_reserve_exact(1)
            .map_err(uqa_core::memory::MemoryError::from)?;
        let started = active
            .iter()
            .rev()
            .find(|context| context.connection == identity)
            .map_or_else(Instant::now, |context| context.started);
        active.push(Context {
            connection: identity,
            cancellation: [control, read]
                .map(|control| control.map(|control| control.cancellation().clone())),
            started,
            interrupted: false,
        });
        Ok(())
    })?;
    let guard = RegistryOperation {
        connection,
        previous_timeout: Duration::from_millis(u64::from(milliseconds)),
    };
    connection
        .busy_handler(Some(busy))
        .map_err(|error| registry_error("install cancellable registry wait", &error))?;
    connection
        .progress_handler(1_024, Some(progress))
        .map_err(|error| registry_error("install registry cancellation", &error))?;
    Ok(Some(guard))
}

pub(super) fn interrupted() -> bool {
    ACTIVE.with(|active| {
        active
            .borrow()
            .last()
            .is_some_and(|context| context.interrupted)
    })
}

fn progress() -> bool {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(context) = active.last() else {
            return false;
        };
        let interrupted = cancellation_requested(&active, context.connection);
        active.last_mut().expect("active operation").interrupted = interrupted;
        interrupted
    })
}

fn cancellation_requested(active: &[Context], connection: usize) -> bool {
    active
        .iter()
        .rev()
        .filter(|context| context.connection == connection)
        .any(|context| {
            context.interrupted
                || context
                    .cancellation
                    .iter()
                    .flatten()
                    .any(uqa_core::CancellationToken::is_cancelled)
        })
}

fn busy(_attempt: i32) -> bool {
    #[cfg(test)]
    BUSY_OBSERVER.with(|observer| {
        if let Some(sender) = observer.borrow().as_ref() {
            let _ = sender.try_send(());
        }
    });
    let remaining = || {
        ACTIVE.with(|active| {
            active.borrow().last().map_or(Duration::ZERO, |context| {
                REGISTRY_BUSY_TIMEOUT.saturating_sub(context.started.elapsed())
            })
        })
    };
    let delay = remaining();
    if progress() || delay.is_zero() {
        return false;
    }
    std::thread::park_timeout(Duration::from_millis(2).min(delay));
    !progress() && !remaining().is_zero()
}

pub(super) fn schema_error(error: String) -> StorageBackendError {
    if interrupted() {
        uqa_core::QueryCancelled.into()
    } else {
        StorageBackendError::Other(error)
    }
}

impl Drop for RegistryOperation<'_> {
    fn drop(&mut self) {
        let restore_nested = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            active.pop();
            let restore = active
                .iter()
                .any(|context| context.connection == std::ptr::from_ref(self.connection) as usize);
            if active.is_empty() {
                *active = Vec::new();
            }
            restore
        });
        if restore_nested {
            let _ = self.connection.busy_handler(Some(busy));
            let _ = self.connection.progress_handler(1_024, Some(progress));
        } else {
            let _ = self.connection.progress_handler(0, None::<fn() -> bool>);
            let _ = self.connection.busy_timeout(self.previous_timeout);
        }
    }
}
