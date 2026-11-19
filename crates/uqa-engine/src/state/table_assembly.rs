//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native table owners enter the same runtime state without detaching their payloads.

use super::{CatalogCell, CatalogGeneration, TableDefinitionCell, TableMetadataCell};
use crate::TableState;
use std::sync::atomic::{AtomicBool, AtomicU64};
use uqa_analysis::AnalyzerDefinition;
use uqa_core::memory::MemoryReservation;
use uqa_execution::catalog::index::value::ValueIndexes;
use uqa_sql::{
    ast::ColumnDef,
    catalog::{
        security::BoundTableSecurity,
        table_definition::{NativeTableConstraints, TableDefinition},
        table_metadata::TableMetadata,
    },
};
use uqa_storage::{
    memory_capture::MemoryCaptureCell, read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance, vector_index::VectorIndexes, DocumentStore,
    InvertedIndex, StorageBackendResult, StorageHandle,
};

/// Definitions and physical owners are complete before the surrounding table is published. Each native field keeps its own allocation lifetime through future snapshots and mutations.
pub(crate) struct TableContents {
    pub object_id: [u8; 16],
    pub storage_generation: [u8; 16],
    pub security: TableMetadata<BoundTableSecurity>,
    pub documents: StorageHandle<dyn DocumentStore>,
    pub text: StorageHandle<dyn InvertedIndex>,
    pub vectors: VectorIndexes,
    pub values: ValueIndexes,
    pub fts_fields: TableMetadata<Vec<String>>,
    pub columns: TableDefinition<Vec<ColumnDef>>,
    pub constraints: NativeTableConstraints,
    pub analyzer: AnalyzerDefinition,
    pub next_id: u128,
    pub maps_integer_keys: bool,
    pub statistics: uqa_planner::ColumnStatistics,
    pub statistics_loaded: bool,
    pub statistics_dirty: bool,
    pub maintenance: StatisticsMaintenance,
}

impl TableContents {
    /// Admit only the new state and independently shared catalog headers. Native definitions and handles already own their payloads; no population traversal, serialization or resource compilation occurs here. Failure consumes the complete unpublished candidate.
    pub(crate) fn into_state(
        self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<TableState> {
        let admission = Admission(control);
        admission.check()?;
        let memory = admission.table_header()?;
        let columns_declared = self
            .constraints
            .columns_declared
            .unwrap_or(!self.columns.is_empty());
        let table = TableState {
            _image_memory: memory,
            lifecycle_id: AtomicU64::new(crate::next_table_lifecycle_id()),
            object_id: self.object_id,
            security: TableMetadataCell::from_generation(admission.generation(self.security)?),
            storage_generation: admission.cell(self.storage_generation)?,
            document_store: MemoryCaptureCell::new(self.documents),
            inverted_index: MemoryCaptureCell::new(self.text),
            vector_indexes: MemoryCaptureCell::new(self.vectors),
            value_indexes: MemoryCaptureCell::new(self.values),
            fts_fields: TableMetadataCell::from_generation(admission.generation(self.fts_fields)?),
            columns: TableDefinitionCell::from_generation(admission.generation(self.columns)?),
            columns_declared: admission.cell(columns_declared)?,
            next_id: admission.cell(self.next_id)?,
            maps_integer_keys: admission.cell(self.maps_integer_keys)?,
            analyzer: admission.cell(self.analyzer)?,
            column_stats: admission.cell(self.statistics)?,
            column_stats_loaded: admission.cell(self.statistics_loaded)?,
            column_stats_dirty: admission.cell(self.statistics_dirty)?,
            statistics_maintenance: admission.cell(self.maintenance)?,
            table_checks: TableDefinitionCell::from_generation(
                admission.generation(self.constraints.checks)?,
            ),
            foreign_keys: TableMetadataCell::from_generation(
                admission.generation(self.constraints.foreign_keys)?,
            ),
            key_constraints: TableMetadataCell::from_generation(
                admission.generation(self.constraints.key_constraints)?,
            ),
            hierarchy: TableDefinitionCell::from_generation(
                admission.generation(self.constraints.hierarchy)?,
            ),
            doc_count_cache: AtomicU64::new(0),
            doc_count_dirty: AtomicBool::new(true),
            persistence: self.constraints.persistence,
            on_commit: self.constraints.on_commit,
            catalog_oids: self.constraints.catalog_oids,
            row_type_array_name: TableMetadataCell::from_generation(
                admission.generation(self.constraints.row_type_array_name)?,
            ),
            dropped_attributes: TableMetadataCell::from_generation(
                admission.generation(self.constraints.dropped_attributes)?,
            ),
        };
        admission.check()?;
        Ok(table)
    }
}

struct Admission<'a>(Option<&'a StorageReadControl>);

impl Admission<'_> {
    fn check(&self) -> StorageBackendResult<()> {
        if let Some(control) = self.0 {
            control.check()?;
        }
        Ok(())
    }

    fn table_header(&self) -> StorageBackendResult<Option<MemoryReservation>> {
        Ok(self
            .0
            .map(|control| control.memory().reserve(size_of::<TableState>()))
            .transpose()?)
    }

    fn generation<T>(&self, value: T) -> StorageBackendResult<CatalogGeneration<T>> {
        self.check()?;
        match self.0 {
            Some(control) => Ok(CatalogGeneration::with_budget(value, control.memory())?),
            None => Ok(CatalogGeneration::new(value)),
        }
    }

    fn cell<T: Send + Sync + 'static, F: Send + Sync + 'static>(
        &self,
        value: T,
    ) -> StorageBackendResult<CatalogCell<T, F>> {
        Ok(CatalogCell::from_snapshot(self.generation(value)?))
    }
}

#[cfg(test)]
mod tests;
