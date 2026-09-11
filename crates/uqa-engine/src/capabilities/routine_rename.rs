//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine rename publication to catalog state, its transaction, and dependent object services.

use crate::{Engine, StorageBackendResult};
use uqa_execution::routines::rename::{self, RoutineRenameContext, RoutineRenameDependents};
use uqa_sql::{
    ast::{FunctionBinding, RenameRoutineStmt},
    SQLError,
};

impl Engine {
    fn routine_rename_context(&self) -> RoutineRenameContext<'_> {
        RoutineRenameContext {
            mutation: self.routine_mutation_context(),
            refresh: self,
            schemas: self,
            compilation: self.stored_routine_compilation_context(),
            dependents: self,
        }
    }
    pub(crate) fn rename_sql_routine(&self, stmt: &RenameRoutineStmt) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            rename::rename_sql_routine(&engine.routine_rename_context(), stmt)
        })
    }
}
impl RoutineRenameDependents for Engine {
    fn rewrite_schema_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        Engine::rewrite_schema_routine_identity(self, target, new_name)
    }
    fn rewrite_view_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        Engine::rewrite_view_routine_identity(self, target, new_name)
    }
    fn rewrite_event_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> Result<(), SQLError> {
        Engine::rewrite_event_routine_identity(self, target, new_name)
    }
}
