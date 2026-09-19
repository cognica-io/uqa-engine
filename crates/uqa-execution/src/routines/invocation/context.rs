//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation inputs retain the caller's live state without exposing an engine.
use crate::routines::RoutineContext;
use uqa_sql::{
    routines::{
        compilation::RoutineParserCatalog, declaration::RoutineTypeCatalog,
        resolution::RoutineOverloadContext, security::RoutineExecutionAuthority, RoutineResolution,
    },
    SQLError,
};
pub trait RoutineInvocationState {
    fn finish(&mut self);
}
pub trait RoutineInvocationSession {
    fn depth_limit(&self) -> usize;
    fn state_guard(
        &self,
        configured: bool,
        security_definer: bool,
    ) -> Box<dyn RoutineInvocationState + '_>;
    fn set_current_user(&self, user: uqa_core::catalog_role::RoleIdentity) -> Result<(), SQLError>;
    fn set_configured_parameter(&self, name: &str, value: &str) -> Result<(), SQLError>;
}
pub struct RoutineInvocationContext<'a> {
    pub runtime: RoutineContext<'a>,
    pub session: &'a dyn RoutineInvocationSession,
    pub lookup: &'a dyn RoutineResolution,
    pub overloads: RoutineOverloadContext<'a>,
    pub types: &'a dyn RoutineTypeCatalog,
    pub authority: &'a dyn RoutineExecutionAuthority,
}
pub struct AnonymousBlockContext<'a> {
    pub runtime: RoutineContext<'a>,
    pub session: &'a dyn RoutineInvocationSession,
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
}
