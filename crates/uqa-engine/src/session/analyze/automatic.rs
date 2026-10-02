//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the analysis relation lock through read sampling, validation and short physical publication.

mod sampling;
#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering;

use uqa_sql::ast::{ColumnType, GeneratedColumnKind};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::statistics::{now_ms, MaintenanceState};
use crate::{ColumnStatsMap, Engine};

/// The data generation of each table a sample read, in scan order.
type DataGenerations = Vec<(String, Option<u64>)>;

struct AutomaticAnalysis {
    object_id: [u8; 16],
    maintenance: MaintenanceState,
    /// The data generation of every table the sample read, where the provider reports generations. A committed row write advances its table's generation, whether or not it rewrote the maintenance record.
    data_generations: Option<DataGenerations>,
    /// The commit sequence the sample read at, where commits are numbered.
    sampled_at: Option<u64>,
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
        if self.storage.backend.is_none() {
            return Ok(false);
        }
        self.with_storage_maintenance_scope(|engine| {
            let Some(target) = uqa_execution::maintenance::analyze::prepare_optional_target(
                &engine.analyze_execution_context(false),
                name,
            )
            .map_err(|error| StorageBackendError::backend("automatic ANALYZE locking", error))?
            else {
                return Ok(false);
            };
            let Some(analysis) = engine.collect_automatic_analysis(&target.name)? else {
                return Ok(false);
            };
            engine.publish_automatic_analysis(&target.name, analysis)
        })
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
        if !maintenance.due(
            missing,
            now_ms(),
            crate::statistics::value_size::FORMAT_VERSION,
        ) {
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
        // Both are read before the sample: a write that commits in between advances a generation past the recorded one, and the sample reads at the recorded sequence or a later one.
        let data_generations = self.sampled_data_generations(name)?;
        let sampled_at = self.statistics_sample_sequence();
        let (statistics, row_count) = sampling::collect(self, name, &columns)?;
        Ok(Some(AutomaticAnalysis {
            object_id: table.object_id(),
            maintenance,
            data_generations,
            sampled_at,
            row_count,
            statistics,
        }))
    }

    /// The data generations of the tables an analysis of `name` samples, or `None` when the provider reports no generations and every commit rewrites the maintenance record instead.
    fn sampled_data_generations(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<DataGenerations>> {
        let Some(catalog) = self.storage.catalog.as_deref() else {
            return Ok(None);
        };
        let Some(revisions) = catalog.cache_revisions()? else {
            return Ok(None);
        };
        let members = self
            .hierarchy_scan_tables(name, true)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        Ok(Some(
            members
                .into_iter()
                .map(|member| {
                    let generation = revisions.table_data.get(&member).copied();
                    (member, generation)
                })
                .collect(),
        ))
    }

    fn publish_automatic_analysis(
        &self,
        name: &str,
        analysis: AutomaticAnalysis,
    ) -> StorageBackendResult<bool> {
        self.with_storage_maintenance_scope(|engine| {
            let Some(target) = uqa_execution::maintenance::analyze::prepare_optional_target(
                &engine.analyze_execution_context(false),
                name,
            )
            .map_err(|error| StorageBackendError::backend("automatic ANALYZE locking", error))?
            else {
                return Ok(false);
            };
            if target.object_id != analysis.object_id {
                return Ok(false);
            }
            let name = target.name.as_str();
            if !engine.try_prepare_storage_maintenance_writer()? {
                return Ok(false);
            }
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
                || engine.sampled_data_generations(name)? != analysis.data_generations
            {
                return Ok(false);
            }
            Self::persist_column_stats(
                catalog,
                name,
                &analysis.statistics,
                table.object_id(),
                analysis.row_count,
                analysis.sampled_at,
            )?;
            *table.column_stats.write() = analysis.statistics;
            table.column_stats_loaded.store(true, Ordering::Release);
            table.column_stats_dirty.store(false, Ordering::Release);
            engine.note_table_data_changed();
            Ok(true)
        })
    }
}
