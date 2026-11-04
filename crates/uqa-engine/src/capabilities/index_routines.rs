//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine-dependent index reads to the live registry and its catalog provider.

use crate::Engine;
use uqa_execution::schema::indexes::routines::{
    self, IndexRoutineContext, IndexRoutineRead, IndexRoutineRegistry,
};
use uqa_sql::ast::FunctionBinding;
use uqa_storage::StorageBackendResult;

impl Engine {
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
    fn publish_routine_indexes(
        &self,
        rows: uqa_execution::catalog::index::registry::CatalogIndexes,
    ) {
        *self.durable.catalog_indexes.write() = rows;
    }
}
