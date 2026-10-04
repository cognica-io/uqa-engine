//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation inputs retain the caller's live state without exposing an engine.
use crate::routines::RoutineContext;
use std::sync::Arc;
use uqa_sql::{
    routines::{
        compilation::{RoutineCompilationContext, RoutineParserCatalog},
        declaration::RoutineTypeCatalog,
        resolution::RoutineOverloadContext,
        security::RoutineExecutionAuthority,
        CompiledFunctionBody, RoutineResolution, SQLUserFunction,
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
    /// The body this session compiled at an earlier call of exactly this definition, as each `PostgreSQL` backend keeps the functions it compiled.
    fn compiled_routine_body(
        &self,
        function: &Arc<SQLUserFunction>,
    ) -> Option<Arc<CompiledFunctionBody>>;
    /// Keep the body this session compiled for later calls of exactly this definition.
    fn retain_compiled_routine_body(
        &self,
        function: &Arc<SQLUserFunction>,
        body: Arc<CompiledFunctionBody>,
    );
}
pub struct RoutineInvocationContext<'a> {
    pub runtime: RoutineContext<'a>,
    pub session: &'a dyn RoutineInvocationSession,
    pub lookup: &'a dyn RoutineResolution,
    pub overloads: RoutineOverloadContext<'a>,
    pub types: &'a dyn RoutineTypeCatalog,
    pub authority: &'a dyn RoutineExecutionAuthority,
    /// The catalog a body that `CREATE FUNCTION` left unexamined compiles against when it is first called.
    pub compilation: RoutineCompilationContext<'a>,
}
pub struct AnonymousBlockContext<'a> {
    pub runtime: RoutineContext<'a>,
    pub session: &'a dyn RoutineInvocationSession,
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
}
