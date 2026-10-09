//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A coalesced background maintenance session per database, never per query.

mod cache;
#[cfg(test)]
mod tests;

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
    statistics: &Weak<super::StatisticsCoordinator>,
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
    let mut diskann = None;
    let mut maintenance = cache::MaintenanceCache::default();
    let mut poll_at = Instant::now();
    loop {
        let pass_started = Instant::now();
        let refresh_due = pass_started >= poll_at;
        if refresh_due {
            poll_at = pass_started + POLL;
        }
        let mut continue_diskann = false;
        if cancellation.is_cancelled() {
            return;
        }
        let Some(manager) = manager.upgrade() else {
            return;
        };
        let Some(statistics) = statistics.upgrade() else {
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
                    engine.statistics = Arc::clone(&statistics);
                    engine.runtime.cancellation = cancellation.clone();
                    session = Some(engine);
                }
                Err(error) => {
                    statistics.automatic_statistics.status.lock().last_error =
                        Some(error.to_string());
                }
            }
        }
        if let Some(engine) = session.as_ref() {
            // Finite DiskANN pages share the worker without restarting the
            // whole-catalog statistics pass before its next poll.
            if refresh_due {
                statistics.automatic_statistics.status.lock().running = true;
                let result = refresh_due_tables(engine, &mut maintenance);
                let mut status = statistics.automatic_statistics.status.lock();
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
            match step_diskann(engine, &mut diskann) {
                Ok(pending) => continue_diskann = pending,
                Err(error) => statistics.diskann.lock().last_error = Some(error.to_string()),
            }
        }
        drop(statistics);
        drop(manager);
        if continue_diskann {
            continue;
        }
        if !wait_until(receiver, cancellation, poll_at) {
            return;
        }
    }
}

fn step_diskann(
    engine: &Engine,
    maintenance: &mut Option<uqa_execution::maintenance::diskann::DiskANNJournalMaintenance>,
) -> StorageBackendResult<bool> {
    let Some(backend) = engine.storage.backend.as_deref() else {
        return Ok(false);
    };
    if maintenance.is_none() {
        let control = engine.query_retention_control().map_err(|error| {
            uqa_storage::StorageBackendError::backend("DiskANN maintenance resources", error)
        })?;
        *maintenance = Some(
            uqa_execution::maintenance::diskann::DiskANNJournalMaintenance::with_rebuilds(
                &control,
                &engine.session.diskann_temporary,
                engine.diskann_rebuild_policy(),
            )?,
        );
    }
    let maintenance = maintenance.as_mut().expect("maintenance was initialized");
    maintenance.set_rebuild_policy(engine.diskann_rebuild_policy())?;
    let version = backend.change_version()?.map(|_| {
        engine
            .epochs
            .seen_storage_change_version
            .load(Ordering::Acquire)
    });
    let result = maintenance.step(
        engine.durable.catalog_indexes.snapshot(),
        version,
        engine,
        backend,
    );
    *engine.statistics.diskann.lock() = maintenance.status();
    result.map(|()| maintenance.has_pending_work())
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

fn refresh_due_tables(
    engine: &Engine,
    maintenance: &mut cache::MaintenanceCache,
) -> StorageBackendResult<()> {
    refresh_due_tables_at(engine, maintenance, now_ms())
}

fn change_version(engine: &Engine) -> StorageBackendResult<Option<u64>> {
    engine
        .storage
        .backend
        .as_ref()
        .map(|backend| backend.change_version())
        .transpose()
        .map(Option::flatten)
}

fn refresh_due_tables_at(
    engine: &Engine,
    maintenance: &mut cache::MaintenanceCache,
    now: u64,
) -> StorageBackendResult<()> {
    let before = change_version(engine)?;
    if maintenance.unchanged(before, now) {
        return Ok(());
    }
    maintenance.begin_pass();
    engine.synchronize_table_catalog()?;
    engine.synchronize_table_data()?;
    let tables = engine
        .storage
        .tables
        .read()
        .iter()
        .map(|(relation, table)| (relation.qualified_name(), Arc::clone(table)))
        .collect::<Vec<_>>();
    maintenance.retain(tables.iter().map(|(name, _)| name.as_str()));
    let revisions = engine.epochs.storage_cache_revisions.lock().clone();
    let version = change_version(engine)?.filter(|version| {
        *version
            == engine
                .epochs
                .seen_storage_change_version
                .load(Ordering::Acquire)
    });
    let mut failure = None;
    let mut next_due_at = None;
    for (name, table) in tables {
        engine
            .runtime
            .cancellation
            .check()
            .map_err(|error| uqa_storage::StorageBackendError::Other(error.to_string()))?;
        let result = (|| {
            let Some(catalog) = engine.storage.catalog.as_deref() else {
                return Ok(false);
            };
            let revision = version.and(revisions.as_ref()).map(|revisions| {
                revisions
                    .statistics_maintenance
                    .get(&name)
                    .copied()
                    .unwrap_or_default()
            });
            let state = maintenance.load(&name, table.object_id(), revision, || {
                let state = MaintenanceState::load_for(catalog, &name, table.object_id())?;
                // A commit racing the metadata read leaves this entry uncached until the next synchronized pass.
                let stable = version.is_some() && change_version(engine)? == version;
                Ok((state, stable))
            })?;
            let missing = state.missing(table.column_stats.read().is_empty());
            let deadline =
                state.next_due_at(missing, now, crate::statistics::value_size::FORMAT_VERSION);
            if let Some(deadline) = deadline {
                next_due_at = Some(next_due_at.map_or(deadline, |next: u64| next.min(deadline)));
            }
            if deadline.is_none_or(|deadline| now < deadline) {
                return Ok(false);
            }
            engine.run_automatic_analyze(&name)
        })();
        match result {
            Ok(true) => {
                maintenance.remove(&name);
                let mut status = engine.statistics.automatic_statistics.status.lock();
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
    if let Some(error) = failure {
        return Err(error);
    }
    // Bracket the whole pass, including its table list and metadata reads.
    // An analysis that committed or a racing writer requires another pass.
    if let Some(version) = version.filter(|version| Some(*version) == before) {
        if change_version(engine)? == before {
            maintenance.complete_pass(version, next_due_at);
        }
    }
    Ok(())
}
