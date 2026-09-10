//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database-shared statistics state independent of the lock implementation.
use super::{AutomaticStatistics, StatisticsSnapshots};
use crate::{row_locks::RowLockManager, ColumnStatsMap};
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock, Weak};

pub(crate) struct StatisticsCoordinator {
    // Retain the coordination identity while this shared statistics state exists.
    _identity: Arc<RowLockManager>,
    column_stats: RwLock<BTreeMap<String, ColumnStatsMap>>,
    pub(super) automatic_statistics: AutomaticStatistics,
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
        column_stats: RwLock::new(BTreeMap::new()),
        automatic_statistics: AutomaticStatistics::default(),
        statistics_snapshots: StatisticsSnapshots::default(),
    });
    coordinators.insert(key, Arc::downgrade(&coordinator));
    coordinator
}

impl StatisticsCoordinator {
    pub(crate) fn publish_column_stats(&self, table: String, stats: ColumnStatsMap) {
        self.column_stats.write().insert(table, stats);
    }
    pub(crate) fn invalidate_column_stats(&self, table: &str) {
        self.column_stats.write().remove(table);
    }
    pub(crate) fn published_column_stats(&self, table: &str) -> Option<ColumnStatsMap> {
        self.column_stats.read().get(table).cloned()
    }
}
