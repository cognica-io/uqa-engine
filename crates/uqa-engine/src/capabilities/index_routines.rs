//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine-dependent index reads to the live registry and its catalog provider.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::indexes::routines::{
    self, IndexRoutineContext, IndexRoutineRead, IndexRoutineRegistry,
};
use uqa_sql::{ast::FunctionBinding, SQLError};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};

impl Engine {
    pub(crate) fn indexes_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<RelationIdentity>, SQLError> {
        routines::indexes_depending_on_routine(self, target)
    }
    pub(crate) fn rewrite_index_routine_identity(
        &self,
        target: &FunctionBinding,
        name: &str,
    ) -> StorageBackendResult<()> {
        routines::rewrite_index_routine_identity(
            &IndexRoutineContext {
                registry: self,
                catalog: self.storage.catalog.as_deref(),
            },
            target,
            name,
        )
    }
}
impl IndexRoutineRegistry for Engine {
    fn routine_index_rows(&self) -> IndexRoutineRead<'_> {
        Box::new(self.durable.catalog_indexes.read())
    }
    fn publish_routine_index_row(&self, row: CatalogIndexRow) {
        self.durable
            .catalog_indexes
            .write()
            .insert(row.relation.clone(), row);
    }
}
