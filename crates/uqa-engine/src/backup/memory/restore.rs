//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded native table owners enter the ordinary Engine state boundary before publication.

mod catalogs;
mod database;
mod input;
mod metadata;
mod resources;

use crate::{
    state::{CatalogCell, CatalogGeneration, StorageContext},
    TableState,
};
use metadata::Metadata;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
use uqa_execution::catalog::table_image::MemoryTableData;
use uqa_sql::{
    ast::{OnCommitAction, RelationPersistence},
    catalog::relation_oids::RelationCatalogOids,
};
use uqa_storage::{
    read_control::StorageReadControl, DocumentStore, InvertedIndex, StorageBackendResult,
};

fn assemble(
    state: &StorageContext,
    metadata: Metadata,
    data: MemoryTableData,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<TableState>> {
    control.check()?;
    let registry = state
        .memory_capture
        .as_ref()
        .expect("private memory destination");
    let properties = metadata.properties;
    let persistence = match properties.persistence {
        0 => RelationPersistence::Permanent,
        1 => RelationPersistence::Unlogged,
        _ => unreachable!("decoded permanent table persistence"),
    };
    let on_commit = match properties.on_commit {
        0 => OnCommitAction::PreserveRows,
        1 => OnCommitAction::DeleteRows,
        2 => OnCommitAction::Drop,
        _ => unreachable!("decoded table on-commit action"),
    };
    let memory = control.memory().reserve(size_of::<TableState>())?;
    let docs: Box<dyn DocumentStore> = Box::new(data.documents);
    let text: Box<dyn InvertedIndex> = Box::new(data.text);
    let table = TableState {
        _image_memory: Some(memory),
        lifecycle_id: AtomicU64::new(crate::next_table_lifecycle_id()),
        object_id: properties.object_id,
        security: metadata.security,
        storage_generation: cell(properties.storage_generation, control)?,
        document_store: registry.cell_with_control(docs, control)?,
        inverted_index: registry.cell_with_control(text, control)?,
        vector_indexes: registry.cell_with_control(data.vectors, control)?,
        value_indexes: registry.cell_with_control(data.values, control)?,
        fts_fields: metadata.fts_fields,
        columns: metadata.columns,
        columns_declared: cell(properties.columns_declared, control)?,
        next_id: cell(properties.next_id, control)?,
        maps_integer_keys: cell(properties.maps_integer_keys, control)?,
        analyzer: cell(metadata.analyzer, control)?,
        column_stats: cell(metadata.statistics, control)?,
        column_stats_loaded: cell(properties.statistics.loaded, control)?,
        column_stats_dirty: cell(properties.statistics.dirty, control)?,
        statistics_maintenance: cell(metadata.maintenance, control)?,
        table_checks: metadata.checks,
        foreign_keys: metadata.foreign_keys,
        key_constraints: metadata.keys,
        hierarchy: metadata.hierarchy,
        doc_count_cache: AtomicU64::new(0),
        doc_count_dirty: AtomicBool::new(true),
        persistence,
        on_commit,
        catalog_oids: properties.catalog_oids.map(|value| RelationCatalogOids {
            relation: value.relation,
            row_type: value.row_type,
            array_type: value.array_type,
            rule: value.rule,
        }),
        row_type_array_name: metadata.array_name,
        dropped_attributes: metadata.dropped,
    };
    let table = state.table_state_with_control(table, control)?;
    control.check()?;
    Ok(table)
}

fn cell<T: Send + Sync + 'static, F: Send + Sync + 'static>(
    value: T,
    control: &StorageReadControl,
) -> StorageBackendResult<CatalogCell<T, F>> {
    Ok(CatalogCell::from_snapshot(CatalogGeneration::with_budget(
        value,
        control.memory(),
    )?))
}

#[cfg(test)]
mod tests;
