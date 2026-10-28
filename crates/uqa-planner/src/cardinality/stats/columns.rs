//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable column collections retain their existing catalog or builder allocation.

use super::{BTreeMap, ColumnStats};
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Column statistics shared by cost estimators, relation descriptors and catalog readers. An absent collection is empty without a heap allocation; a captured catalog collection retains its original immutable generation.
#[derive(Debug, Clone, Default)]
pub struct ColumnStatistics(Option<Arc<BTreeMap<String, ColumnStats>>>);

impl From<BTreeMap<String, ColumnStats>> for ColumnStatistics {
    fn from(columns: BTreeMap<String, ColumnStats>) -> Self {
        Self((!columns.is_empty()).then(|| Arc::new(columns)))
    }
}

impl From<Arc<BTreeMap<String, ColumnStats>>> for ColumnStatistics {
    fn from(columns: Arc<BTreeMap<String, ColumnStats>>) -> Self {
        Self(Some(columns))
    }
}

impl FromIterator<(String, ColumnStats)> for ColumnStatistics {
    fn from_iter<T: IntoIterator<Item = (String, ColumnStats)>>(columns: T) -> Self {
        BTreeMap::from_iter(columns).into()
    }
}

impl std::ops::Deref for ColumnStatistics {
    type Target = BTreeMap<String, ColumnStats>;

    fn deref(&self) -> &Self::Target {
        static EMPTY: BTreeMap<String, ColumnStats> = BTreeMap::new();
        self.0.as_deref().unwrap_or(&EMPTY)
    }
}

impl ColumnStatistics {
    /// Standalone statistics builders detach from a shared generation before changing its contents. Native costing only observes the completed collection.
    pub(super) fn insert(&mut self, name: String, stats: ColumnStats) {
        let columns = self.0.get_or_insert_with(|| Arc::new(BTreeMap::new()));
        Arc::make_mut(columns).insert(name, stats);
    }
}
