//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained table-state adapters for initial typed-value restoration.

use super::{Engine, StorageBackendError, StorageBackendResult};
use std::sync::atomic::Ordering;

impl uqa_execution::catalog::value_restoration::ValueRestorationSession for Engine {
    fn index_build_context(&self) -> uqa_execution::schema::indexes::IndexBuildContext<'_> {
        self.index_creation_context().unique
    }

    fn rebuild_value_indexes_and_refresh_statistics(
        &self,
        table: &str,
    ) -> StorageBackendResult<()> {
        let state = self.try_table(table)?.ok_or_else(|| {
            StorageBackendError::Other(format!("restored value owner `{table}` disappeared"))
        })?;
        state.value_indexes.write().clear();
        state.column_stats.write().clear();
        state.column_stats_loaded.store(true, Ordering::Release);
        state.column_stats_dirty.store(true, Ordering::Release);
        self.refresh_value_indexes_for_table(table)
    }
}
