//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind hierarchy execution to active metadata and transaction locks.
use crate::Engine;
use uqa_execution::schema::hierarchy::{HierarchyCatalog, HierarchyContext, HierarchyNamespace};
use uqa_sql::{
    ast::{
        ColumnDef, ForeignKey, RelationPersistence, TableCheck, TableConstraintSet,
        TableKeyConstraint,
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn hierarchy_execution_context(&self) -> HierarchyContext<'_> {
        HierarchyContext {
            catalog: self,
            namespace: self,
            constraints: self.constraint_execution_context(),
            partitions: self.partition_context(),
            publication: self.schema_publication_context(),
        }
    }
}
impl HierarchyCatalog for Engine {
    fn try_describe_table(&self, table: &str) -> StorageBackendResult<Option<Vec<ColumnDef>>> {
        Engine::try_describe_table(self, table)
    }
    fn try_check_constraint_definitions(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<TableCheck>> {
        Engine::try_check_constraint_definitions(self, table)
    }
    fn try_key_constraints(&self, table: &str) -> StorageBackendResult<Vec<TableKeyConstraint>> {
        Engine::try_key_constraints(self, table)
    }
    fn try_foreign_keys(&self, table: &str) -> StorageBackendResult<Vec<ForeignKey>> {
        Engine::try_foreign_keys(self, table)
    }
    fn try_declared_table_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<TableConstraintSet> {
        Engine::try_declared_table_constraints(self, table)
    }
    fn table_persistence(&self, table: &str) -> StorageBackendResult<Option<RelationPersistence>> {
        Engine::table_persistence(self, table)
    }
}
impl HierarchyNamespace for Engine {
    fn resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<uqa_execution::catalog::RelationResolution, SQLError> {
        Engine::resolve_visible_relation_kind(self, name)
    }
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError> {
        self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)
    }
}
