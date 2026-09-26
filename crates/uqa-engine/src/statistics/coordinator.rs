//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database-shared statistics state independent of the lock implementation.
use super::{AutomaticStatistics, StatisticsSnapshots};
use crate::row_locks::RowLockManager;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

pub(crate) struct StatisticsCoordinator {
    // Retain the coordination identity while this shared statistics state exists.
    _identity: Arc<RowLockManager>,
    pub(super) automatic_statistics: AutomaticStatistics,
    pub(super) diskann: Mutex<uqa_execution::maintenance::diskann::DiskANNMaintenanceStatus>,
    pub(super) diskann_policy: Mutex<uqa_execution::maintenance::diskann::DiskANNRebuildPolicy>,
    pub(crate) statistics_snapshots: StatisticsSnapshots,
}

static COORDINATORS: OnceLock<Mutex<HashMap<usize, Weak<StatisticsCoordinator>>>> = OnceLock::new();

pub(crate) fn shared_statistics(identity: &Arc<RowLockManager>) -> Arc<StatisticsCoordinator> {
    let key = Arc::as_ptr(identity) as usize;
    let mut coordinators = COORDINATORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    coordinators.retain(|_, entry| entry.strong_count() != 0);
    if let Some(coordinator) = coordinators.get(&key).and_then(Weak::upgrade) {
        return coordinator;
    }
    let coordinator = Arc::new(StatisticsCoordinator {
        _identity: Arc::clone(identity),
        automatic_statistics: AutomaticStatistics::default(),
        diskann: Mutex::default(),
        diskann_policy: Mutex::default(),
        statistics_snapshots: StatisticsSnapshots::default(),
    });
    coordinators.insert(key, Arc::downgrade(&coordinator));
    coordinator
}
