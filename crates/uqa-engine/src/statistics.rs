//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable automatic-statistics scheduling, independent of query planning.

mod cache;
mod coordinator;
pub(crate) use coordinator::{shared_statistics, StatisticsCoordinator};
pub(crate) mod value_size;
mod worker;
pub(crate) use cache::StatisticsSnapshots;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use uqa_storage::StorageBackendResult;

use crate::{Engine, TableState};

pub(crate) use uqa_storage::statistics_maintenance::StatisticsMaintenance as MaintenanceState;

#[derive(Clone, Default)]
pub(crate) struct StatisticsChange {
    object_id: [u8; 16],
    count: u64,
    reset: bool,
}

pub(crate) type StatisticsChanges = BTreeMap<String, StatisticsChange>;

/// Process-local health of the database's automatic statistics worker.
/// A failed refresh retains its durable pending state and is retried.
#[derive(Clone, Debug, Default)]
pub struct AutomaticStatisticsStatus {
    pub running: bool,
    pub completed: u64,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct AutomaticStatistics {
    clients: AtomicUsize,
    control: Mutex<Option<StatisticsWorker>>,
    status: Mutex<AutomaticStatisticsStatus>,
}

struct StatisticsWorker {
    sender: mpsc::SyncSender<()>,
    cancellation: uqa_core::CancellationToken,
    thread: std::thread::JoinHandle<()>,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

impl Engine {
    pub fn automatic_statistics_status(&self) -> AutomaticStatisticsStatus {
        self.statistics.automatic_statistics.status.lock().clone()
    }

    pub(crate) fn record_statistics_change(
        &self,
        name: &str,
        table: &TableState,
        count: u64,
    ) -> StorageBackendResult<()> {
        if self.storage.catalog.is_none()
            || table.persistence == uqa_sql::ast::RelationPersistence::Temporary
        {
            return Ok(());
        }
        let mut stack = self.session.transactions.lock();
        if let Some(frame) = stack.last_mut() {
            let change = frame.statistics_changes.entry(name.to_owned()).or_default();
            if change.object_id != table.object_id() {
                *change = StatisticsChange {
                    object_id: table.object_id(),
                    count: 0,
                    reset: false,
                };
            }
            change.count = change.count.saturating_add(count);
            return Ok(());
        }
        drop(stack);
        let changes = BTreeMap::from([(
            name.to_owned(),
            StatisticsChange {
                object_id: table.object_id(),
                count,
                reset: false,
            },
        )]);
        self.persist_statistics_changes(&changes)?;
        self.wake_automatic_statistics();
        Ok(())
    }

    pub(crate) fn persist_statistics_changes(
        &self,
        changes: &StatisticsChanges,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.storage.catalog.as_deref() else {
            return Ok(());
        };
        let tables = self.storage.tables.read();
        for (name, change) in changes {
            if change.count == 0 {
                continue;
            }
            // DROP/recreate and rename cannot attach the old relation's
            // pending maintenance to a different durable object.
            let Some(table) = tables.iter().find_map(|(relation, table)| {
                (relation.qualified_name() == *name && table.object_id() == change.object_id)
                    .then_some(table)
            }) else {
                continue;
            };
            let mut state = MaintenanceState::load_for(catalog, name, change.object_id)?;
            state.record_changes(
                change.object_id,
                change.count,
                table
                    .column_stats
                    .read()
                    .values()
                    .next()
                    .map(|stats| stats.row_count),
                now_ms(),
            )?;
            state.save(catalog, name)?;
        }
        Ok(())
    }

    pub(crate) fn merge_statistics_changes(
        into: &mut StatisticsChanges,
        changes: StatisticsChanges,
    ) {
        for (name, change) in changes {
            let previous = into.entry(name).or_default();
            if previous.object_id == change.object_id && !change.reset {
                previous.count = previous.count.saturating_add(change.count);
            } else {
                *previous = change;
            }
        }
    }

    pub(crate) fn clear_pending_statistics_changes(&self, name: &str, object_id: [u8; 16]) {
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            // A child analysis covers its parent's pending changes only if the child commits. Preserve the parent entry across a child rollback.
            frame.statistics_changes.insert(
                name.to_owned(),
                StatisticsChange {
                    object_id,
                    count: 0,
                    reset: true,
                },
            );
        }
    }

    pub(crate) fn start_automatic_statistics(&self) {
        let Some(provider) = self.storage.provider.as_ref() else {
            return;
        };
        if self.session.statistics_worker.load(Ordering::Acquire) {
            return;
        }
        let automatic = &self.statistics.automatic_statistics;
        if !self.session.statistics_client.swap(true, Ordering::AcqRel) {
            automatic.clients.fetch_add(1, Ordering::AcqRel);
        }
        let mut control = automatic.control.lock();
        if let Some(worker) = control.as_ref() {
            if worker.sender.try_send(()) != Err(mpsc::TrySendError::Disconnected(())) {
                return;
            }
            *control = None;
        }
        let (requests, receiver) = mpsc::sync_channel(1);
        let provider = Arc::downgrade(provider);
        let manager = Arc::downgrade(&self.row_locks);
        let statistics = Arc::downgrade(&self.statistics);
        let cancellation = uqa_core::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        match std::thread::Builder::new()
            .name("uqa-auto-analyze".into())
            .spawn(move || {
                worker::run(
                    &provider,
                    &manager,
                    &statistics,
                    &receiver,
                    &worker_cancellation,
                );
            }) {
            Ok(thread) => {
                *control = Some(StatisticsWorker {
                    sender: requests,
                    cancellation,
                    thread,
                });
            }
            Err(error) => {
                automatic.status.lock().last_error = Some(error.to_string());
            }
        }
    }

    pub(crate) fn wake_automatic_statistics(&self) {
        if self.session.statistics_worker.load(Ordering::Acquire) {
            return;
        }
        self.start_automatic_statistics();
        if let Some(worker) = self.statistics.automatic_statistics.control.lock().as_ref() {
            // One pending wake-up is enough: the durable counter is the
            // source of truth, so coalescing never loses committed changes.
            let _ = worker.sender.try_send(());
        }
    }

    pub(crate) fn release_automatic_statistics_client(&self) {
        if self.session.statistics_client.swap(false, Ordering::AcqRel) {
            let automatic = &self.statistics.automatic_statistics;
            if automatic.clients.fetch_sub(1, Ordering::AcqRel) != 1 {
                return;
            }
            let worker = {
                let mut control = automatic.control.lock();
                if automatic.clients.load(Ordering::Acquire) != 0 {
                    return;
                }
                control.take()
            };
            if let Some(worker) = worker {
                worker.cancellation.cancel();
                let _ = worker.sender.try_send(());
                // Release every maintenance connection before the final
                // client's drop returns. redb requires this for immediate
                // reopen; SQLite requires it for final WAL checkpointing.
                if worker.thread.join().is_err() {
                    automatic.status.lock().last_error =
                        Some("automatic statistics worker panicked".into());
                }
            }
        }
    }
}
