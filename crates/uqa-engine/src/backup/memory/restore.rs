//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded native table owners enter the ordinary Engine state boundary before publication.

mod catalogs;
pub(in crate::backup) mod database;
mod input;
mod metadata;
mod resources;

use crate::{
    state::{StorageContext, TableContents},
    TableState,
};
use metadata::Metadata;
use std::sync::Arc;
use uqa_core::memory::Budgeted;
use uqa_execution::catalog::table_image::MemoryTableData;
use uqa_sql::{
    ast::{OnCommitAction, RelationPersistence},
    catalog::{relation_oids::RelationCatalogOids, table_definition::NativeTableConstraints},
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
    let memory = control.memory().reserve(size_of_val(&data.documents))?;
    let documents: Box<dyn DocumentStore> = Box::new(data.documents);
    let documents = Budgeted::new(documents, memory).into();
    let memory = control.memory().reserve(size_of_val(&data.text))?;
    let text: Box<dyn InvertedIndex> = Box::new(data.text);
    let text = Budgeted::new(text, memory).into();
    let contents = TableContents {
        object_id: properties.object_id,
        storage_generation: properties.storage_generation,
        security: metadata.security,
        documents,
        text,
        vectors: data.vectors,
        values: data.values,
        fts_fields: metadata.fts_fields,
        columns: metadata.columns,
        constraints: NativeTableConstraints {
            columns_declared: Some(properties.columns_declared),
            checks: metadata.checks,
            foreign_keys: metadata.foreign_keys,
            key_constraints: metadata.keys,
            hierarchy: metadata.hierarchy,
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
        },
        analyzer: metadata.analyzer,
        next_id: properties.next_id,
        maps_integer_keys: properties.maps_integer_keys,
        statistics: metadata.statistics,
        statistics_loaded: properties.statistics.loaded,
        statistics_dirty: properties.statistics.dirty,
        maintenance: metadata.maintenance,
    };
    state.table_state(contents.into_state(Some(control))?, Some(control))
}

#[cfg(test)]
mod tests;
