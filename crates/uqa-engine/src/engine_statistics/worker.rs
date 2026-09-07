//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A coalesced background maintenance session per database, never per query.

use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Weak};
use std::time::{Duration, Instant};

use uqa_storage::{PersistentStorageProvider, StorageBackendResult};

use super::{now_ms, MaintenanceState};
use crate::{row_locks::RowLockManager, Engine};

const POLL: Duration = Duration::from_secs(1);

pub(super) fn run(
    provider: &Weak<dyn PersistentStorageProvider>,
    manager: &Weak<RowLockManager>,
    receiver: &mpsc::Receiver<()>,
    cancellation: &uqa_core::CancellationToken,
) {
    // Opening and restoring this one maintenance session is off the app's
    // startup/statement path. Short-lived engines need not open it at all.
    if !wait_until(receiver, cancellation, Instant::now() + POLL) {
        return;
    }
    let Some(provider) = provider.upgrade() else {
        return;
    };
    let mut session = None;
    loop {
        let pass_started = Instant::now();
        if cancellation.is_cancelled() {
            return;
        }
        let Some(manager) = manager.upgrade() else {
            return;
        };
        if session.is_none() {
            match provider.open_session().and_then(|storage| {
                Engine::from_initialized_persistent_session(storage, Some(Arc::clone(&provider)))
            }) {
                Ok(mut engine) => {
                    engine
                        .session
                        .statistics_worker
                        .store(true, Ordering::Release);
                    engine.session_id = manager.allocate_session();
                    engine.row_locks = Arc::clone(&manager);
                    engine.runtime.cancellation = cancellation.clone();
                    session = Some(engine);
                }
                Err(error) => {
                    manager.automatic_statistics.status.lock().last_error = Some(error.to_string());
                }
            }
        }
        if let Some(engine) = session.as_ref() {
            manager.automatic_statistics.status.lock().running = true;
            let result = refresh_due_tables(engine);
            let mut status = manager.automatic_statistics.status.lock();
            status.running = false;
            match result {
                Ok(()) => {
                    status.last_error = None;
                }
                Err(error) => {
                    status.last_error = Some(error.to_string());
                }
            }
        }
        drop(manager);
        if !wait_until(receiver, cancellation, pass_started + POLL) {
            return;
        }
    }
}

fn wait_until(
    receiver: &mpsc::Receiver<()>,
    cancellation: &uqa_core::CancellationToken,
    deadline: Instant,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return true;
        };
        if receiver.recv_timeout(remaining) == Err(mpsc::RecvTimeoutError::Disconnected) {
            return false;
        }
    }
}

fn refresh_due_tables(engine: &Engine) -> StorageBackendResult<()> {
    engine.synchronize_table_catalog()?;
    engine.synchronize_table_data()?;
    let names = engine
        .storage
        .tables
        .read()
        .keys()
        .map(uqa_storage::RelationIdentity::qualified_name)
        .collect::<Vec<_>>();
    let mut failure = None;
    for name in names {
        engine
            .runtime
            .cancellation
            .check()
            .map_err(|error| uqa_storage::StorageBackendError::Other(error.to_string()))?;
        let result = (|| {
            let Some(table) = engine.try_table(&name)? else {
                return Ok(false);
            };
            let Some(catalog) = engine.storage.catalog.as_deref() else {
                return Ok(false);
            };
            let state = MaintenanceState::load_for(catalog, &name, table.object_id())?;
            let missing = state.analyzed_rows.is_none() && table.column_stats.read().is_empty();
            if !state.due(missing, now_ms()) {
                return Ok(false);
            }
            engine.run_automatic_analyze(&name)
        })();
        match result {
            Ok(true) => {
                let mut status = engine.row_locks.automatic_statistics.status.lock();
                status.completed = status.completed.saturating_add(1);
            }
            Ok(false) => {}
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
