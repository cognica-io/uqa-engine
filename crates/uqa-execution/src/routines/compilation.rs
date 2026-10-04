//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile stored routine bodies inside their recorded creation namespace.

use super::configuration::RoutineConfigurationGuard;
use std::sync::Arc;
use uqa_sql::{
    ast::{CreateFunction, FunctionBody},
    routines::{
        compilation::{self as analysis, RoutineCompilationContext},
        CompiledFunctionBody, RoutineBody,
    },
    SQLError,
};

/// Complete a `PL/pgSQL` compilation with the session's settings for what the body does not declare, as a backend compiles a function under the settings in effect.
pub fn apply_session_compile_options(
    session: &dyn super::invocation::context::RoutineInvocationSession,
    parsed: &mut uqa_sql::plpgsql::PLpgSQLFunction,
) {
    parsed.variable_conflict = parsed
        .options
        .variable_conflict
        .unwrap_or_else(|| session.plpgsql_variable_conflict());
}

/// Examine a body given as a string under the routine's own settings, as `PostgreSQL` validates it under them at creation and compiles it under them when the routine is called.
pub fn with_routine_settings<T>(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
    examine: impl FnOnce() -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let _settings = context.session.routine_settings_scope(&def.config)?;
    examine()
}

pub trait RoutineCompilationSession {
    fn routine_search_path(&self) -> Vec<String>;
    fn replace_routine_search_path(&self, path: Vec<String>) -> Vec<String>;
    fn restore_routine_search_path(&self, path: Vec<String>);
    /// Apply a routine's own settings, as a call of the routine applies them, until the returned scope is dropped.
    fn routine_settings_scope(
        &self,
        settings: &[(String, String)],
    ) -> Result<Box<dyn RoutineConfigurationGuard + '_>, SQLError>;
}
#[derive(Clone, Copy)]
pub struct StoredRoutineCompilationContext<'a> {
    pub analysis: RoutineCompilationContext<'a>,
    pub session: &'a dyn RoutineCompilationSession,
}

pub fn compile_persisted_sql_function(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    if !matches!(def.body, FunctionBody::Statements(_)) || def.creation_search_path.is_empty() {
        return analysis::compile_persisted_function_body(&context.analysis, def);
    }
    let previous = context
        .session
        .replace_routine_search_path(def.creation_search_path.clone());
    let compiled = analysis::compile_persisted_function_body(&context.analysis, def);
    context.session.restore_routine_search_path(previous);
    compiled
}

/// The body the catalog keeps for a stored definition: a SQL-standard body bound again, and a source body left to the sessions that run it.
pub fn persisted_routine_body(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<RoutineBody, SQLError> {
    match def.body {
        FunctionBody::Statements(_) => compile_persisted_sql_function(context, def)
            .map(|body| RoutineBody::Bound(Arc::new(body))),
        FunctionBody::Source(_) => Ok(RoutineBody::Source),
    }
}
