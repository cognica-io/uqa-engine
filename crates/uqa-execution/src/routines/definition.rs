//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture routine namespaces, bind catalog dependencies, and recompile changed definitions.

use super::compilation::{self, StoredRoutineCompilationContext};
use uqa_sql::{
    ast::{CreateFunction, FunctionBody},
    binding::stored_columns::StoredSourceCatalog,
    routines::{
        body_parameters::record_sql_standard_body_parameters,
        compilation::{compile_function_body, defer_function_body},
        dependencies::{self, RoutineCompilationMode},
        regclass::{self, RoutineRegclassCatalog},
        CompiledFunctionBody,
    },
    SQLError,
};

pub struct RoutineDefinitionContext<'a> {
    pub compilation: StoredRoutineCompilationContext<'a>,
    pub sources: &'a dyn StoredSourceCatalog,
    pub regclasses: &'a dyn RoutineRegclassCatalog,
}

/// How a routine's body is compiled with its definition. A SQL-standard body belongs to the statement that creates the routine, so it is always compiled and bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutineBodyCompilation {
    /// `CREATE FUNCTION` under `check_function_bodies`: a body given as a string is compiled under the routine's own settings, and its errors are reported.
    Checked,
    /// `CREATE FUNCTION` with `check_function_bodies` off: a body given as a string is stored unexamined.
    Unchecked,
    /// A stored definition: a body given as a string is left for each session to compile when it first calls the routine, as each `PostgreSQL` backend compiles a function at its first call, so that a body that no longer compiles reports its error then.
    Stored,
}

impl RoutineBodyCompilation {
    const fn mode(self) -> RoutineCompilationMode {
        match self {
            Self::Checked | Self::Unchecked => RoutineCompilationMode::Definition,
            Self::Stored => RoutineCompilationMode::Persisted,
        }
    }
}

pub fn compile_catalog_bound_routine(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    bodies: RoutineBodyCompilation,
) -> Result<(CompiledFunctionBody, bool), SQLError> {
    let mode = bodies.mode();
    if matches!(mode, RoutineCompilationMode::Definition) {
        if matches!(def.body, FunctionBody::Statements(_))
            || def
                .params
                .iter()
                .any(|parameter| parameter.default.is_some())
        {
            def.creation_search_path = context.compilation.session.routine_search_path();
        } else {
            def.creation_search_path.clear();
        }
    }
    let mut changed = bind_routine_definition_dependencies(context, def, mode)?;
    if matches!(mode, RoutineCompilationMode::Definition) {
        changed |= record_sql_standard_body_parameters(&context.compilation.analysis, def)?;
    }
    let mut compiled = compile_routine_body(&context.compilation, def, bodies)?;
    let body_changed = {
        let dependency_body = compilation::stored_merge_dependency_body(&context.compilation, def)?;
        dependencies::bind_sql_standard_body_routines(
            &context.compilation.analysis,
            def,
            dependency_body.as_ref().unwrap_or(&compiled),
        )
    }? | bind_routine_regclass_constants(context, def)?;
    changed |= body_changed;
    if body_changed {
        compiled = compile_routine_body(&context.compilation, def, bodies)?;
    }
    Ok((compiled, changed))
}

fn compile_routine_body(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
    bodies: RoutineBodyCompilation,
) -> Result<CompiledFunctionBody, SQLError> {
    match (bodies, &def.body) {
        (
            RoutineBodyCompilation::Checked | RoutineBodyCompilation::Unchecked,
            FunctionBody::Statements(_),
        ) => compile_function_body(&context.analysis, def),
        (RoutineBodyCompilation::Stored, FunctionBody::Statements(_)) => {
            compilation::compile_persisted_sql_function(context, def)
        }
        (RoutineBodyCompilation::Checked, FunctionBody::Source(_)) => {
            compilation::with_routine_settings(context, def, || {
                compile_function_body(&context.analysis, def)
            })
        }
        (RoutineBodyCompilation::Unchecked, FunctionBody::Source(_)) => {
            defer_function_body(&context.analysis, def)
        }
        (RoutineBodyCompilation::Stored, FunctionBody::Source(_)) => {
            Ok(CompiledFunctionBody::Deferred)
        }
    }
}

fn bind_routine_definition_dependencies(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    let bind = |def: &mut CreateFunction| {
        dependencies::bind_routine_definition_dependencies(
            &context.compilation.analysis,
            context.sources,
            def,
            mode,
        )
    };
    if def.creation_search_path.is_empty() {
        return bind(def);
    }
    let previous = context
        .compilation
        .session
        .replace_routine_search_path(def.creation_search_path.clone());
    let result = bind(def);
    context
        .compilation
        .session
        .restore_routine_search_path(previous);
    result
}

fn bind_routine_regclass_constants(
    context: &RoutineDefinitionContext<'_>,
    definition: &mut CreateFunction,
) -> Result<bool, SQLError> {
    let previous = context
        .compilation
        .session
        .replace_routine_search_path(definition.creation_search_path.clone());
    let result = regclass::bind_routine_regclass_constants(
        context.compilation.analysis.types,
        context.regclasses,
        definition,
    );
    context
        .compilation
        .session
        .restore_routine_search_path(previous);
    result
}
