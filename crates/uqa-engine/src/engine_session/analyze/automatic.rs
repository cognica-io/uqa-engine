//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sample with a read snapshot, then validate and publish in a short write.

mod sampling;
#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering;

use uqa_sql::ast::{ColumnType, GeneratedColumnKind};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::engine_statistics::{now_ms, MaintenanceState};
use crate::{ColumnStatsMap, Engine};

struct AutomaticAnalysis {
    object_id: [u8; 16],
    maintenance: MaintenanceState,
    row_count: u64,
    statistics: ColumnStatsMap,
}

fn automatic_column(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { base, .. } => automatic_column(base),
        ColumnType::Bytea
        | ColumnType::Vector(_)
        | ColumnType::Tensor(_)
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Array(_)
        | ColumnType::AnyArray
        | ColumnType::Record => false,
        _ => true,
    }
}

impl Engine {
    pub(crate) fn run_automatic_analyze(&self, name: &str) -> StorageBackendResult<bool> {
        let _statement = self.runtime.statement_gate.lock();
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(false);
        };
        backend.begin_read_transaction()?;
        let result = self
            .refresh_pinned_transaction_snapshot()
            .and_then(|()| self.collect_automatic_analysis(name));
        // Release the read snapshot before acquiring a writer, including on
        // rollback-journal storage. Only this independent worker's gate is held.
        let rollback = backend.rollback_transaction();
        let analysis = match (result, rollback) {
            (Ok(analysis), Ok(())) => analysis,
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(error), Err(rollback)) => {
                return Err(StorageBackendError::Other(format!(
                    "automatic analysis failed: {error}; read cleanup failed: {rollback}"
                )))
            }
        };
        let Some(analysis) = analysis else {
            return Ok(false);
        };
        self.publish_automatic_analysis(name, analysis)
    }

    fn collect_automatic_analysis(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<AutomaticAnalysis>> {
        let Some(table) = self.try_table(name)? else {
            return Ok(None);
        };
        let Some(catalog) = self.storage.catalog.as_deref() else {
            return Ok(None);
        };
        let maintenance = MaintenanceState::load_for(catalog, name, table.object_id())?;
        let missing = maintenance.missing(table.column_stats.read().is_empty());
        if !maintenance.due(missing, now_ms()) {
            return Ok(None);
        }
        let columns = table
            .columns
            .read()
            .iter()
            .filter(|column| {
                automatic_column(&column.ty)
                    && !column
                        .generated
                        .as_ref()
                        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
            })
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let (statistics, row_count) = sampling::collect(self, name, &columns)?;
        Ok(Some(AutomaticAnalysis {
            object_id: table.object_id(),
            maintenance,
            row_count,
            statistics,
        }))
    }

    fn publish_automatic_analysis(
        &self,
        name: &str,
        analysis: AutomaticAnalysis,
    ) -> StorageBackendResult<bool> {
        self.with_read_only_compatible_storage_transaction(|engine| {
            let Some(table) = engine.try_table(name)? else {
                return Ok(false);
            };
            let Some(catalog) = engine.storage.catalog.as_deref() else {
                return Ok(false);
            };
            // Identity and generation must still match. A concurrent write,
            // explicit ANALYZE, or DROP/recreate leaves newer work pending.
            if table.object_id() != analysis.object_id
                || MaintenanceState::load_for(catalog, name, table.object_id())?
                    != analysis.maintenance
            {
                return Ok(false);
            }
            Self::persist_column_stats(catalog, name, &analysis.statistics)?;
            MaintenanceState::analyzed_for(catalog, name, table.object_id(), analysis.row_count)?;
            *table.column_stats.write() = analysis.statistics;
            table.column_stats_loaded.store(true, Ordering::Release);
            table.column_stats_dirty.store(false, Ordering::Release);
            engine.note_table_data_changed();
            Ok(true)
        })
    }
}
