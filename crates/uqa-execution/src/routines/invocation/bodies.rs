//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned routine source compilation, retained until its definition changes. SQL statement input analysis has a separate dependency-aware lifetime; source syntax stays available for reanalysis. The caches are not transactional.

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
    sql_inputs: crate::routines::sql_body::inputs::SQLRoutineInputs,
}

impl SessionRoutineBodies {
    pub fn sql_inputs(&self) -> &crate::routines::sql_body::inputs::SQLRoutineInputs {
        &self.sql_inputs
    }

    /// The body the session executes: a bound body as defined, or the session's compilation of a source body, compiled when the session first needs it and again after the routine's catalog entry changed.
    pub fn body(
        &self,
        function: &SQLUserFunction,
        compile: impl FnOnce(&CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        if let Some(body) = self.retained(function)? {
            return Ok(body);
        }
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
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

    /// Inspect retained source compilation if it exists, otherwise compile a
    /// temporary body without initializing the runtime cache. An unreached call
    /// therefore keeps its original execution-time compilation boundary.
    pub fn inspect(
        &self,
        function: &SQLUserFunction,
        compile: impl FnOnce(&CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        self.retained(function)?
            .map_or_else(|| compile(&function.def).map(Arc::new), Ok)
    }

    fn retained(
        &self,
        function: &SQLUserFunction,
    ) -> Result<Option<Arc<CompiledFunctionBody>>, SQLError> {
        if let RoutineBody::Bound(body) = &function.body {
            return Ok(Some(Arc::clone(body)));
        }
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
        Ok(self
            .compiled
            .lock()
            .get(&identity)
            .filter(|compiled| compiled.version == version)
            .map(|compiled| Arc::clone(&compiled.body)))
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

/// Read a SQL source body for effects or dependencies without making it the
/// backend's executable body. Static inspection does not run the function and
/// therefore neither emits its lexical warnings nor initializes its cache.
pub fn compile_analysis_body(
    session: &dyn RoutineInvocationSession,
    compilation: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    struct AnalysisParser<'a>(&'a dyn uqa_sql::routines::compilation::RoutineParserCatalog);
    impl uqa_sql::routines::compilation::RoutineParserCatalog for AnalysisParser<'_> {
        fn plpgsql_catalog(&self) -> Result<uqa_sql::plpgsql::PlpgsqlCatalog, SQLError> {
            self.0.plpgsql_catalog()
        }
        fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
            self.0.parser_settings()
        }
    }
    let parsers = AnalysisParser(compilation.parsers);
    let analysis = RoutineCompilationContext {
        parsers: &parsers,
        ..*compilation
    };
    compile_session_body(session, &analysis, def)
}

#[cfg(test)]
mod tests;
