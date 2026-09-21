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
        TableKeyConstraint, TableLockMode,
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
            constraint_access: self,
            constraint_modes: self,
        }
    }
}

impl uqa_execution::schema::hierarchy::detachment::DetachedConstraintModes for Engine {
    fn preserve_split_modes(
        &self,
        retained: &[uqa_sql::catalog::constraints::ConstraintIdentity],
        detached: &[uqa_sql::schema::inheritance::detachment::ConstraintIdentityChange],
    ) -> Result<(), SQLError> {
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            uqa_sql::schema::inheritance::detachment::preserve_split_constraint_modes(
                &mut frame.constraint_modes.named,
                retained,
                detached,
            );
        }
        self.prune_constraint_modes()
    }
}
impl HierarchyCatalog for Engine {
    fn try_describe_table(&self, table: &str) -> StorageBackendResult<Option<Vec<ColumnDef>>> {
        Engine::describe_table_in_execution(self, table)
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
    fn lock_relation(&self, table: &str, mode: TableLockMode) -> Result<(), SQLError> {
        self.lock_relation(table, mode.into())
    }
}
