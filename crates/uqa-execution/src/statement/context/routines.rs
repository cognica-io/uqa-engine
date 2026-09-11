//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native routine definition, privilege, invocation and retained write scopes.

use crate::routines::{
    invocation::context::{AnonymousBlockContext, RoutineInvocationContext},
    privileges::RoutinePrivilegeContext,
    registration::RoutineRegistrationContext,
    removal::context::RoutineRemovalContext,
    rename::RoutineRenameContext,
};
use uqa_sql::{routines::RoutineResolution, SQLError};
pub trait RoutineStatementInputs {
    fn invocation_context(&self) -> RoutineInvocationContext<'_>;
    fn anonymous_block_context(&self) -> AnonymousBlockContext<'_>;
    fn registration_context(&self) -> RoutineRegistrationContext<'_>;
    fn privilege_context(&self) -> RoutinePrivilegeContext<'_>;
}
pub type RoutineRenameWrite<'a> =
    Box<dyn FnOnce(&RoutineRenameContext<'_>) -> Result<(), SQLError> + 'a>;
pub type RoutineRemovalWrite<'a> =
    Box<dyn FnOnce(&RoutineRemovalContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait RoutineStatementTransactions {
    fn with_rename(&self, operation: RoutineRenameWrite<'_>) -> Result<(), SQLError>;
    fn with_removal(&self, operation: RoutineRemovalWrite<'_>) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct RoutineStatements<'a> {
    pub resolution: &'a dyn RoutineResolution,
    pub inputs: &'a dyn RoutineStatementInputs,
    pub transactions: &'a dyn RoutineStatementTransactions,
}
