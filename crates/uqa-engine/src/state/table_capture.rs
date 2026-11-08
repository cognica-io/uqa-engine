//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Newly created table metadata joins the same source domain as its data before publication.

use super::{CatalogCell, StorageContext};
use crate::TableState;
use std::sync::Arc;
use uqa_storage::StorageBackendResult;

impl StorageContext {
    pub(crate) fn table_state(
        &self,
        mut table: TableState,
    ) -> StorageBackendResult<Arc<TableState>> {
        let Some(registry) = self.table_capture_registry(table.persistence) else {
            return Ok(Arc::new(table));
        };
        macro_rules! capture {
            ($field:ident, $copy:expr) => {
                table.$field =
                    CatalogCell::capture_source(table.$field.snapshot(), Some(registry), $copy)?;
            };
        }
        table.columns = table.columns.capture_source(registry)?;
        table.table_checks = table.table_checks.capture_source(registry)?;
        table.hierarchy = table.hierarchy.capture_source(registry)?;
        table.foreign_keys = table.foreign_keys.capture_source(registry)?;
        table.key_constraints = table.key_constraints.capture_source(registry)?;
        table.fts_fields = table.fts_fields.capture_source(registry)?;
        table.row_type_array_name = table.row_type_array_name.capture_source(registry)?;
        table.dropped_attributes = table.dropped_attributes.capture_source(registry)?;
        capture!(columns_declared, |value, control| Ok(
            control.finish(*value, control.empty_reservation())?
        ));
        table.security = table.security.capture_source(registry)?;
        capture!(
            column_stats,
            uqa_planner::ColumnStatistics::capture_with_control
        );
        capture!(analyzer, |value, control| {
            value
                .capture_image(control)
                .map_err(|error| uqa_sql::SQLError::Host(Arc::new(error)))
        });
        capture!(storage_generation, inline);
        capture!(next_id, inline);
        capture!(maps_integer_keys, inline);
        capture!(column_stats_loaded, inline);
        capture!(column_stats_dirty, inline);
        capture!(statistics_maintenance, |value, control| Ok(
            value.clone_with_control(control)?
        ));
        Ok(Arc::new(table))
    }
}

fn inline<T: Copy>(
    value: &T,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<T>, uqa_sql::SQLError> {
    Ok(control.finish(*value, control.empty_reservation())?)
}
