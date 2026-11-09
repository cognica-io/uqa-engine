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
    pub(crate) fn table_state(&self, table: TableState) -> StorageBackendResult<Arc<TableState>> {
        self.register_table_state(table, None)
    }

    pub(crate) fn table_state_with_control(
        &self,
        table: TableState,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<Arc<TableState>> {
        self.register_table_state(table, Some(control))
    }

    fn register_table_state(
        &self,
        mut table: TableState,
        control: Option<&uqa_storage::read_control::StorageReadControl>,
    ) -> StorageBackendResult<Arc<TableState>> {
        let Some(registry) = self.table_capture_registry(table.persistence) else {
            return Ok(Arc::new(table));
        };
        macro_rules! capture {
            ($field:ident, $copy:expr) => {
                table.$field = match control {
                    Some(control) => CatalogCell::capture_source_with_control(
                        table.$field.snapshot(),
                        registry,
                        $copy,
                        control,
                    )?,
                    None => {
                        CatalogCell::capture_source(table.$field.snapshot(), Some(registry), $copy)?
                    }
                };
            };
        }
        macro_rules! native {
            ($($field:ident),* $(,)?) => {$(
                table.$field = match control {
                    Some(control) => table.$field.capture_source_with_control(registry, control)?,
                    None => table.$field.capture_source(registry)?,
                };
            )*};
        }
        native!(
            columns,
            table_checks,
            hierarchy,
            foreign_keys,
            key_constraints,
            fts_fields,
            row_type_array_name,
            dropped_attributes,
            security
        );
        capture!(columns_declared, |value, control| Ok(
            control.finish(*value, control.empty_reservation())?
        ));
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
