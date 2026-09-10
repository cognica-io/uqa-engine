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
use serde::{Deserialize, Serialize};
use uqa_storage::{CatalogFacade, StorageBackendResult};

use crate::{Engine, TableState};

const MAX_DIRTY_AGE_MS: u64 = 60_000;

#[derive(Clone, Default)]
pub(crate) struct StatisticsChange {
    object_id: [u8; 16],
    count: u64,
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

/// Transactional maintenance metadata. Existing column statistics remain
/// available while this record tracks changes awaiting a replacement.
#[derive(Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MaintenanceState {
    object_id: Option<[u8; 16]>,
    generation: u64,
    changes: u64,
    dirty_since_ms: u64,
    analyzed_rows: Option<u64>,
    statistics_format: u32,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

impl MaintenanceState {
    fn key(table: &str) -> String {
        format!("uqa.statistics.maintenance.v1:{table}")
    }

    pub(crate) fn load(catalog: &dyn CatalogFacade, table: &str) -> StorageBackendResult<Self> {
        catalog
            .get_metadata(&Self::key(table))?
            .map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn load_for(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
    ) -> StorageBackendResult<Self> {
        let state = Self::load(catalog, table)?;
        Ok(
            if state.object_id.is_some_and(|stored| stored != object_id) {
                Self::default()
            } else {
                state
            },
        )
    }

    fn save(&self, catalog: &dyn CatalogFacade, table: &str) -> StorageBackendResult<()> {
        catalog.set_metadata(&Self::key(table), &serde_json::to_string(self)?)
    }

    pub(crate) fn dirty(&self) -> bool {
        self.changes != 0
    }

    pub(crate) fn missing(&self, statistics_empty: bool) -> bool {
        self.analyzed_rows.is_none() && statistics_empty
    }

    pub(crate) fn invalidates_existing_statistics(&self) -> bool {
        // Initial imported statistics predate this maintenance protocol.
        // A new write adopts that baseline before marking it stale.
        self.dirty() && self.analyzed_rows.is_some()
    }

    pub(crate) fn due(&self, missing: bool, now: u64) -> bool {
        self.statistics_format != value_size::FORMAT_VERSION
            || missing
            || (self.dirty()
                && (self.analyzed_rows == Some(0)
                    || self.changes >= 50 + self.analyzed_rows.unwrap_or(0) / 10
                    || now.saturating_sub(self.dirty_since_ms) >= MAX_DIRTY_AGE_MS))
    }

    pub(crate) fn analyzed(
        catalog: &dyn CatalogFacade,
        table: &str,
        rows: u64,
    ) -> StorageBackendResult<()> {
        let mut state = Self::load(catalog, table)?;
        state.advance_generation()?;
        state.changes = 0;
        state.dirty_since_ms = 0;
        state.analyzed_rows = Some(rows);
        state.statistics_format = value_size::FORMAT_VERSION;
        state.save(catalog, table)
    }

    pub(crate) fn analyzed_for(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
        rows: u64,
    ) -> StorageBackendResult<()> {
        let mut state = Self::load_for(catalog, table, object_id)?;
        state.object_id = Some(object_id);
        state.advance_generation()?;
        state.changes = 0;
        state.dirty_since_ms = 0;
        state.analyzed_rows = Some(rows);
        state.statistics_format = value_size::FORMAT_VERSION;
        state.save(catalog, table)
    }

    fn advance_generation(&mut self) -> StorageBackendResult<()> {
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            uqa_storage::StorageBackendError::Other("statistics generation space exhausted".into())
        })?;
        Ok(())
    }
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
            // DROP/recreate and rename cannot attach the old relation's
            // pending maintenance to a different durable object.
            let Some(table) = tables.iter().find_map(|(relation, table)| {
                (relation.qualified_name() == *name && table.object_id() == change.object_id)
                    .then_some(table)
            }) else {
                continue;
            };
            let mut state = MaintenanceState::load_for(catalog, name, change.object_id)?;
            state.object_id = Some(change.object_id);
            state.advance_generation()?;
            if state.analyzed_rows.is_none() {
                state.analyzed_rows = table
                    .column_stats
                    .read()
                    .values()
                    .next()
                    .map(|stats| stats.row_count);
            }
            if !state.dirty() {
                state.dirty_since_ms = now_ms();
            }
            state.changes = state.changes.saturating_add(change.count);
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
            if previous.object_id == change.object_id {
                previous.count = previous.count.saturating_add(change.count);
            } else {
                *previous = change;
            }
        }
    }

    pub(crate) fn clear_pending_statistics_changes(&self, name: &str) {
        for frame in self.session.transactions.lock().iter_mut() {
            frame.statistics_changes.remove(name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_refresh_covers_first_use_threshold_and_small_idle_changes() {
        assert!(MaintenanceState::default().due(true, 0));
        let mut state = MaintenanceState {
            changes: 1,
            dirty_since_ms: 100,
            analyzed_rows: Some(100),
            statistics_format: value_size::FORMAT_VERSION,
            ..MaintenanceState::default()
        };
        assert!(!state.due(false, 101));
        assert!(state.due(false, 60_100));
        state.changes = 60;
        assert!(state.due(false, 101));
    }

    #[test]
    fn legacy_statistics_are_refreshed_without_waiting_for_another_write() {
        let old: MaintenanceState =
            serde_json::from_str(r#"{"generation":4,"changes":0,"analyzed_rows":120}"#).unwrap();
        assert!(old.due(false, 0));
    }
}
