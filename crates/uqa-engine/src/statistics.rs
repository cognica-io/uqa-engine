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
    /// For changes a session keeps: a commit sequence at or after the last commit that made them, or `None` when its provider numbers no commits. An analysis that sampled at or after it has seen them.
    through: Option<u64>,
}

pub(crate) type StatisticsChanges = BTreeMap<String, StatisticsChange>;

/// What a commit did with the statistics changes of its transaction. The session takes it over once the commit has succeeded: a failed or undone commit leaves what the session kept as it was.
#[derive(Clone, Default)]
pub(crate) struct StatisticsSettlement {
    /// Tables whose maintenance record the transaction wrote, with every change the session had kept of them.
    recorded: Vec<String>,
    /// Changes the session keeps to itself, including the ones it kept before.
    kept: StatisticsChanges,
}

impl StatisticsSettlement {
    /// Whether the transaction wrote a maintenance record, which the automatic statistics worker should see.
    pub(crate) fn recorded(&self) -> bool {
        !self.recorded.is_empty()
    }
}

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

    pub fn automatic_diskann_maintenance_status(&self) -> crate::DiskANNMaintenanceStatus {
        self.statistics.diskann.lock().clone()
    }

    /// Database-shared, process-local rebuild thresholds for future maintenance admissions.
    pub fn diskann_rebuild_policy(&self) -> crate::DiskANNRebuildPolicy {
        *self.statistics.diskann_policy.lock()
    }

    /// Revisit pending changes under this policy at the next maintenance poll. Current builds retain their admitted policy and resource limits; this setting is not persisted.
    pub fn set_diskann_rebuild_policy(&self, policy: crate::DiskANNRebuildPolicy) {
        *self.statistics.diskann_policy.lock() = policy;
        self.start_automatic_statistics();
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
            let previous_rows = table
                .column_stats
                .read()
                .values()
                .next()
                .map(|stats| stats.row_count);
            return table.statistics_maintenance.lock().record_changes(
                table.object_id(),
                count,
                previous_rows,
                now_ms(),
            );
        }
        let mut stack = self.session.transactions.lock();
        if let Some(frame) = stack.last_mut() {
            let change = frame.statistics_changes.entry(name.to_owned()).or_default();
            if change.object_id != table.object_id() {
                *change = StatisticsChange {
                    object_id: table.object_id(),
                    count: 0,
                    reset: false,
                    through: None,
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
                through: None,
            },
        )]);
        let settlement = self.persist_statistics_changes(&changes)?;
        self.settle_statistics_changes(settlement);
        self.start_automatic_statistics();
        Ok(())
    }

    /// Write the maintenance records the changes of a committing transaction call for. A session keeps the changes that decide nothing ([`MaintenanceState::defers`]) to itself and records them with a later commit, where an analysis learns of every committed write from the table's data generation instead of from this record.
    pub(crate) fn persist_statistics_changes(
        &self,
        changes: &StatisticsChanges,
    ) -> StorageBackendResult<StatisticsSettlement> {
        let mut settlement = StatisticsSettlement::default();
        let Some(catalog) = self.storage.catalog.as_deref() else {
            return Ok(settlement);
        };
        // A session keeps changes only where an analysis can do without the record: cache generations tell it of every committed write to what it sampled, and numbered commits tell the session which of its changes the analysis saw.
        let keeps = self.epochs.storage_cache_revisions.lock().is_some()
            && self
                .storage
                .backend
                .as_ref()
                .is_some_and(|backend| backend.transaction_model().is_versioned());
        let kept = self.session.kept_statistics.lock().clone();
        let now = now_ms();
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
            // What the session kept counts unless an analysis has sampled the commits that made it.
            let earlier = kept
                .get(name)
                .filter(|earlier| {
                    earlier.object_id == change.object_id && !state.covers(earlier.through)
                })
                .map_or(0, |earlier| earlier.count);
            let count = earlier.saturating_add(change.count);
            if keeps && state.defers(count, now) {
                settlement.kept.insert(
                    name.clone(),
                    StatisticsChange {
                        object_id: change.object_id,
                        count,
                        reset: false,
                        through: None,
                    },
                );
                continue;
            }
            state.record_changes(
                change.object_id,
                count,
                table
                    .column_stats
                    .read()
                    .values()
                    .next()
                    .map(|stats| stats.row_count),
                now,
            )?;
            state.save(catalog, name)?;
            settlement.recorded.push(name.clone());
        }
        Ok(settlement)
    }

    /// Take over what a successful commit did with its statistics changes. The caller has left the transaction that committed them.
    pub(crate) fn settle_statistics_changes(&self, settlement: StatisticsSettlement) {
        // Read after the commit, so that it lies at or after every commit of the kept changes. A session that is still inside a transaction would read that transaction's earlier view.
        let through = (!settlement.kept.is_empty())
            .then(|| {
                let backend = self.storage.backend.as_ref()?;
                if backend.in_transaction() {
                    return None;
                }
                backend.change_version().ok()?
            })
            .flatten();
        let mut kept = self.session.kept_statistics.lock();
        for name in settlement.recorded {
            kept.remove(&name);
        }
        kept.extend(settlement.kept.into_iter().map(|(name, mut change)| {
            change.through = through;
            (name, change)
        }));
    }

    /// The commit sequence an analysis that samples now reads at, for the record it writes. `None` where commits are not numbered, and in a transaction that reads its data at an earlier fixed snapshot than its storage view.
    pub(crate) fn statistics_sample_sequence(&self) -> Option<u64> {
        let backend = self.storage.backend.as_ref()?;
        if !backend.transaction_model().is_versioned()
            || self.current_transaction_uses_fixed_snapshot()
        {
            return None;
        }
        backend.change_version().ok().flatten()
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
                    through: None,
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
            // Session admission only retains the worker. Sending a liveness
            // probe wakes its wait without advancing the maintenance deadline.
            if !worker.thread.is_finished() {
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
            .name("uqa-maintenance".into())
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
