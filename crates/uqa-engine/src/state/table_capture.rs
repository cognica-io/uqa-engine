//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Newly created table metadata joins the same source domain as its data before publication.

use super::{CatalogCell, StorageContext};
use crate::TableState;
use std::sync::Arc;
use uqa_sql::catalog::definition_capture;
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
        capture!(columns, |value, control| definition_capture::columns(
            value, control
        ));
        capture!(table_checks, |value, control| definition_capture::checks(
            value, control
        ));
        capture!(foreign_keys, |value, control| {
            definition_capture::foreign_keys(value, control)
        });
        capture!(key_constraints, |value, control| definition_capture::keys(
            value, control
        ));
        capture!(hierarchy, definition_capture::hierarchy);
        capture!(fts_fields, |value, control| definition_capture::names(
            value, control
        ));
        capture!(row_type_array_name, definition_capture::optional_name);
        capture!(dropped_attributes, |value, control| {
            definition_capture::dropped_attributes(value, control)
        });
        capture!(columns_declared, |value, control| Ok(
            control.finish(*value, control.empty_reservation())?
        ));
        capture!(security, super::BoundTableSecurity::capture_security);
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
