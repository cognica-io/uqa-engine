//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema and column-statistics restoration.

use super::{BTreeMap, CatalogFacade, Engine, StorageBackendError, StorageBackendResult};
use uqa_planner::stored_column_statistics::{StoredColumnStats, StoredColumnStatsError};

impl Engine {
    pub(super) fn restore_schemas_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: super::CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let roles = self.durable.roles.read();
        let schemas = uqa_execution::schema::namespaces::restoration::restore(
            catalog,
            &roles,
            mode.allows_migration(),
        )?;
        drop(roles);
        *self.durable.schemas.write() = schemas;
        Ok(())
    }

    pub(crate) fn load_column_stats_from_catalog(
        catalog: &dyn CatalogFacade,
        table_name: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        let mut out = BTreeMap::new();
        for row in catalog.load_column_stats(table_name)? {
            let statistics = StoredColumnStats {
                column_name: &row.column_name,
                distinct_count: row.distinct_count,
                null_count: row.null_count,
                min_value: row.min_value.as_deref(),
                max_value: row.max_value.as_deref(),
                row_count: row.row_count,
                histogram_json: &row.histogram_json,
                mcv_values_json: &row.mcv_values_json,
                mcv_frequencies_json: &row.mcv_frequencies_json,
            }
            .decode()
            .map_err(|error| match error {
                StoredColumnStatsError::Json(error) => StorageBackendError::Serde(error),
                StoredColumnStatsError::Resource(error) => error.into(),
                error => StorageBackendError::Other(error.to_string()),
            })?;
            out.insert(row.column_name, statistics);
        }
        Ok(out)
    }
}
