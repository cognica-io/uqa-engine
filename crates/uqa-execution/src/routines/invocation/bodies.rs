//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine bodies compiled by one session. `PostgreSQL` compiles a routine whose body is a string the first time a backend runs it and keeps the compilation for the backend's lifetime until the routine's catalog entry changes: a type renamed afterwards does not affect the backend that compiled the body, while a backend that compiles it later resolves the names again. The cache is not transactional.

use super::context::RoutineInvocationSession;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_sql::ast::CreateFunction;
use uqa_sql::routines::{
    compilation::{compile_function_body, RoutineCompilationContext},
    CompiledFunctionBody, RoutineBody, SQLUserFunction,
};
use uqa_sql::SQLError;

/// The session that keeps the compilation validating a new routine body.
pub trait RoutineBodySession {
    fn retain_routine_body(
        &self,
        function: &SQLUserFunction,
        body: CompiledFunctionBody,
    ) -> Result<(), SQLError>;
}

/// A session's compilation of a routine body and the definition version it was compiled from.
struct CompiledRoutine {
    version: u64,
    body: Arc<CompiledFunctionBody>,
}

/// Compilations of routine source bodies, by routine identity.
#[derive(Default)]
pub struct SessionRoutineBodies {
    compiled: Mutex<BTreeMap<[u8; 16], CompiledRoutine>>,
}

impl SessionRoutineBodies {
    /// The body the session executes: a bound body as defined, or the session's compilation of a source body, compiled when the session first needs it and again after the routine's catalog entry changed.
    pub fn body(
        &self,
        function: &SQLUserFunction,
        compile: impl FnOnce(&CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        if let RoutineBody::Bound(body) = &function.body {
            return Ok(Arc::clone(body));
        }
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
        if let Some(compiled) = self.compiled.lock().get(&identity) {
            if compiled.version == version {
                return Ok(Arc::clone(&compiled.body));
            }
        }
        // Compiling can resolve and compile other routines, so the cache is not held meanwhile.
        let body = Arc::new(compile(&function.def)?);
        self.compiled.lock().insert(
            identity,
            CompiledRoutine {
                version,
                body: Arc::clone(&body),
            },
        );
        Ok(body)
    }

    /// Keep the compilation that validated a new definition, as the PL/pgSQL validator leaves it in the defining backend's function cache.
    pub fn retain(
        &self,
        function: &SQLUserFunction,
        body: CompiledFunctionBody,
    ) -> Result<(), SQLError> {
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
        self.compiled.lock().insert(
            identity,
            CompiledRoutine {
                version,
                body: Arc::new(body),
            },
        );
        Ok(())
    }
}

fn routine_identity(function: &SQLUserFunction) -> Result<[u8; 16], SQLError> {
    function.def.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "routine `{}` has no object identity",
            function.def.name
        ))
    })
}

/// Compile a source body in the routine's own settings and security context, as `fmgr_security_definer` applies them before the language handler compiles the body: a `PL/pgSQL` body takes the `plpgsql.variable_conflict` then in effect unless it declares its own.
pub fn compile_session_body(
    session: &dyn RoutineInvocationSession,
    compilation: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    let compile = || {
        let mut body = compile_function_body(compilation, def)?;
        if let CompiledFunctionBody::PLpgSQL(parsed) = &mut body {
            crate::routines::compilation::apply_session_compile_options(session, parsed);
        }
        Ok(body)
    };
    if def.config.is_empty() && !def.security.security_definer {
        return compile();
    }
    super::scopes::with_routine_context(session, def, compile)
}
