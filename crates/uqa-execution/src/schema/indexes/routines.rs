//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read bound index dependencies and publish rewritten catalog rows after releasing the read guard.

use crate::catalog::index::index_definition;
use crate::catalog::index::registry::CatalogIndexes;
use std::ops::Deref;
use uqa_sql::{
    ast::{FunctionBinding, IndexKey},
    schema::indexes::routines as analysis,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub type IndexRoutineRead<'a> =
    Box<dyn Deref<Target = crate::catalog::index::registry::CatalogIndexes> + 'a>;
pub trait IndexRoutineRegistry {
    fn routine_index_rows(&self) -> IndexRoutineRead<'_>;
    fn publish_routine_indexes(&self, rows: CatalogIndexes);
}
pub struct IndexRoutineContext<'a> {
    pub registry: &'a dyn IndexRoutineRegistry,
    pub catalog: Option<&'a dyn CatalogFacade>,
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
    if updates.is_empty() {
        return Ok(());
    }
    let mut candidate = rows.clone();
    for row in &updates {
        candidate.replace(row.clone())?;
    }
    drop(rows);
    for row in &updates {
        if let Some(catalog) = context
            .catalog
            .filter(|_| candidate.is_durable(&row.relation))
        {
            catalog.save_catalog_index_row(row)?;
        }
    }
    context.registry.publish_routine_indexes(candidate);
    Ok(())
}
