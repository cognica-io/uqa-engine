//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain native routine input capture and the existing catalog transaction scopes.

use crate::Engine;
use uqa_execution::statement::context::routines::{
    RoutineRemovalWrite, RoutineRenameWrite, RoutineStatementInputs, RoutineStatementTransactions,
};
use uqa_sql::SQLError;
impl RoutineStatementInputs for Engine {
    fn invocation_context(
        &self,
    ) -> uqa_execution::routines::invocation::context::RoutineInvocationContext<'_> {
        Engine::routine_invocation_context(self)
    }
    fn anonymous_block_context(
        &self,
    ) -> uqa_execution::routines::invocation::context::AnonymousBlockContext<'_> {
        Engine::anonymous_block_context(self)
    }
    fn registration_context(
        &self,
    ) -> uqa_execution::routines::registration::RoutineRegistrationContext<'_> {
        Engine::routine_registration_context(self)
    }
    fn privilege_context(
        &self,
    ) -> uqa_execution::routines::privileges::RoutinePrivilegeContext<'_> {
        Engine::routine_privilege_context(self)
    }
}
impl RoutineStatementTransactions for Engine {
    fn with_rename(&self, operation: RoutineRenameWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| operation(&engine.routine_rename_context()))
    }
    fn with_removal(&self, operation: RoutineRemovalWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| operation(&engine.routine_removal_context()))
    }
}
