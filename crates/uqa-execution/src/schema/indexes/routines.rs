//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read bound index dependencies and publish rewritten catalog rows after releasing the read guard.

use crate::catalog::index::index_definition;
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{FunctionBinding, IndexKey},
    schema::indexes::routines as analysis,
    SQLError,
};
use uqa_storage::{CatalogFacade, CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub type IndexRoutineRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, CatalogIndexRow>> + 'a>;
pub trait IndexRoutineRegistry {
    fn routine_index_rows(&self) -> IndexRoutineRead<'_>;
    fn publish_routine_index_row(&self, row: CatalogIndexRow);
}
pub struct IndexRoutineContext<'a> {
    pub registry: &'a dyn IndexRoutineRegistry,
    pub catalog: Option<&'a dyn CatalogFacade>,
}

pub fn indexes_depending_on_routine(
    registry: &dyn IndexRoutineRegistry,
    target: &FunctionBinding,
) -> Result<Vec<RelationIdentity>, SQLError> {
    let mut indexes = Vec::new();
    let rows = registry.routine_index_rows();
    for row in rows.values() {
        let definition =
            index_definition(row).map_err(|error| SQLError::Internal(error.to_string()))?;
        let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        if analysis::index_references_routine(&keys, &definition, target)? {
            indexes.push(row.relation.clone());
        }
    }
    Ok(indexes)
}

pub fn rewrite_index_routine_identity(
    context: &IndexRoutineContext<'_>,
    target: &FunctionBinding,
    name: &str,
) -> StorageBackendResult<()> {
    let mut updates = Vec::new();
    let rows = context.registry.routine_index_rows();
    for row in rows.values() {
        let mut definition = index_definition(row)?;
        let mut keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        if analysis::rewrite_index_routine_references(&mut keys, &mut definition, target, name)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
        {
            let mut row = row.clone();
            row.columns_json = serde_json::to_string(&keys)?;
            row.definition_json = Some(serde_json::to_string(&definition)?);
            updates.push(row);
        }
    }
    drop(rows);
    for row in updates {
        if let Some(catalog) = context.catalog {
            catalog.save_catalog_index_row(&row)?;
        }
        context.registry.publish_routine_index_row(row);
    }
    Ok(())
}
