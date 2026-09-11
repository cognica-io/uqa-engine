//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine privilege analysis and publication to current role and session state.

use crate::Engine;
use uqa_execution::routines::privileges::{self, RoutinePrivilegeContext, RoutinePrivilegeNotices};
use uqa_sql::{
    ast::{AlterRoutineOwnerStmt, CreateFunction, GrantRoutineStmt},
    routines::security::{self, RoutineExecutionAuthority},
    SQLError,
};

impl RoutineExecutionAuthority for Engine {
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn current_user_has_role_privileges(&self, role: &str) -> bool {
        Engine::current_user_has_role_privileges(self, role)
    }
}
impl RoutinePrivilegeNotices for Engine {
    fn routine_privilege_notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
impl Engine {
    fn routine_privilege_context(&self) -> RoutinePrivilegeContext<'_> {
        RoutinePrivilegeContext {
            catalog: self.routine_mutation_context(),
            types: self,
            role_names: self,
            notices: self,
        }
    }
    pub(crate) fn alter_sql_routine_owner(
        &self,
        stmt: &AlterRoutineOwnerStmt,
    ) -> Result<(), SQLError> {
        privileges::alter_sql_routine_owner(&self.routine_privilege_context(), stmt)
    }
    pub(crate) fn grant_sql_routine(&self, stmt: &GrantRoutineStmt) -> Result<(), SQLError> {
        privileges::grant_sql_routine(&self.routine_privilege_context(), stmt)
    }
    pub(crate) fn ensure_routine_execute_privilege(
        &self,
        definition: &CreateFunction,
    ) -> Result<(), SQLError> {
        security::ensure_routine_execute_privilege(self, definition)
    }
    pub(crate) fn ensure_routine_execute_privilege_named(
        &self,
        definition: &CreateFunction,
        display_name: &str,
    ) -> Result<(), SQLError> {
        security::ensure_routine_execute_privilege_named(self, definition, display_name)
    }
}
