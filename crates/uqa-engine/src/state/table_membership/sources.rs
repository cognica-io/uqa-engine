//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A table entry preserves immutable lifecycle attributes and weak source identities.

use uqa_core::{memory::MemoryError, RelationIdentity};
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureKey, MemoryCaptureSources},
    StorageBackendResult,
};

use crate::TableState;

macro_rules! table_sources {
    ($($field:ident: $frozen:ty),* $(,)?) => {
        pub(super) struct TableSources {
            pub(super) object_id: [u8; 16],
            pub(super) catalog_oids: Option<uqa_sql::catalog::relation_oids::RelationCatalogOids>,
            pub(super) persistence: uqa_sql::ast::RelationPersistence,
            pub(super) on_commit: uqa_sql::ast::OnCommitAction,
            payload_bytes: usize,
            $(pub(super) $field: MemoryCaptureKey<$frozen>,)*
        }

        #[cfg(test)]
        pub(crate) struct CapturedTable {
            pub(crate) object_id: [u8; 16],
            pub(crate) catalog_oids: Option<uqa_sql::catalog::relation_oids::RelationCatalogOids>,
            pub(crate) persistence: uqa_sql::ast::RelationPersistence,
            pub(crate) on_commit: uqa_sql::ast::OnCommitAction,
            $(pub(crate) $field: std::sync::Arc<uqa_core::memory::Budgeted<$frozen>>,)*
        }

        #[cfg(test)]
        impl CapturedTable {
            pub(crate) fn assert_original_allowance(&self, memory: &uqa_core::memory::MemoryBudget) {
                $(assert!(self.$field.budget().shares_allowance(memory), stringify!($field));)*
            }
        }

        impl TableSources {
            pub(super) fn new(table: &TableState, relation: &RelationIdentity) -> StorageBackendResult<Self> {
                let mut source = Self {
                    object_id: table.object_id(),
                    catalog_oids: table.recorded_catalog_oids(),
                    persistence: table.persistence,
                    on_commit: table.on_commit,
                    payload_bytes: relation.schema.capacity().checked_add(relation.name.capacity()).ok_or(MemoryError::SizeOverflow)?,
                    $($field: table.$field.key()?,)*
                };
                $(source.payload_bytes = source.payload_bytes.checked_add(source.$field.retained_source_bytes()).ok_or(MemoryError::SizeOverflow)?;)*
                Ok(source)
            }

            pub(super) fn payload_bytes(&self) -> usize {
                self.payload_bytes
            }

            pub(super) fn from_snapshot(table: &crate::TableDataSnapshot, relation: &RelationIdentity) -> StorageBackendResult<Self> {
                let mut source = Self {
                    object_id: table.object_id,
                    catalog_oids: table.catalog_oids,
                    persistence: table.persistence,
                    on_commit: table.on_commit,
                    payload_bytes: relation.schema.capacity().checked_add(relation.name.capacity()).ok_or(MemoryError::SizeOverflow)?,
                    $($field: table.$field.key()?,)*
                };
                $(source.payload_bytes = source.payload_bytes.checked_add(source.$field.retained_source_bytes()).ok_or(MemoryError::SizeOverflow)?;)*
                Ok(source)
            }

            pub(super) fn select(&self, selection: &MemoryCaptureSources) -> StorageBackendResult<MemoryCaptureSources> {
                let mut selected = selection.clone();
                $(selected = selected.with_source(&self.$field)?;)*
                Ok(selected)
            }

            #[cfg(test)]
            pub(super) fn assert_unread(&self, cut: &uqa_storage::memory_capture::MemoryCapture) {
                $(assert!(self.$field.retained(cut).unwrap().is_none(), stringify!($field));)*
            }

            pub(super) fn with_name(&self, relation: &RelationIdentity) -> StorageBackendResult<Self> {
                let mut source = Self {
                    object_id: self.object_id,
                    catalog_oids: self.catalog_oids,
                    persistence: self.persistence,
                    on_commit: self.on_commit,
                    payload_bytes: relation.schema.capacity().checked_add(relation.name.capacity()).ok_or(MemoryError::SizeOverflow)?,
                    $($field: self.$field.clone(),)*
                };
                $(source.payload_bytes = source.payload_bytes.checked_add(source.$field.retained_source_bytes()).ok_or(MemoryError::SizeOverflow)?;)*
                Ok(source)
            }

            #[cfg(test)]
            pub(super) fn capture(&self, cut: &uqa_storage::memory_capture::MemoryCapture) -> StorageBackendResult<CapturedTable> {
                cut.check()?;
                let value = CapturedTable {
                    object_id: self.object_id,
                    catalog_oids: self.catalog_oids,
                    persistence: self.persistence,
                    on_commit: self.on_commit,
                    $($field: self.$field.captured(cut)?,)*
                };
                cut.check()?;
                Ok(value)
            }
        }
    }
}

table_sources! {
    document_store: std::sync::Arc<dyn uqa_storage::DocumentStore>,
    inverted_index: std::sync::Arc<dyn uqa_storage::InvertedIndex>,
    vector_indexes: uqa_storage::vector_index::CapturedVectorIndexes,
    value_indexes: uqa_execution::catalog::index::value::CapturedValueIndexes,
    security: CapturedMemoryValue<uqa_sql::catalog::security::table_binding::CapturedTableSecurity>,
    storage_generation: CapturedMemoryValue<[u8; 16]>,
    fts_fields: CapturedMemoryValue<Vec<String>>,
    columns: CapturedMemoryValue<Vec<uqa_sql::ast::ColumnDef>>,
    columns_declared: CapturedMemoryValue<bool>,
    next_id: CapturedMemoryValue<u128>,
    maps_integer_keys: CapturedMemoryValue<bool>,
    analyzer: CapturedMemoryValue<Vec<u8>>,
    column_stats: CapturedMemoryValue<uqa_planner::ColumnStatistics>,
    column_stats_loaded: CapturedMemoryValue<bool>,
    column_stats_dirty: CapturedMemoryValue<bool>,
    statistics_maintenance: CapturedMemoryValue<crate::statistics::MaintenanceState>,
    table_checks: CapturedMemoryValue<Vec<uqa_sql::ast::TableCheck>>,
    foreign_keys: CapturedMemoryValue<Vec<uqa_sql::ast::ForeignKey>>,
    key_constraints: CapturedMemoryValue<Vec<uqa_sql::ast::TableKeyConstraint>>,
    hierarchy: CapturedMemoryValue<uqa_sql::ast::TableHierarchy>,
    row_type_array_name: CapturedMemoryValue<Option<String>>,
    dropped_attributes: CapturedMemoryValue<Vec<uqa_sql::catalog::relation_attributes::DroppedAttribute>>,
}
