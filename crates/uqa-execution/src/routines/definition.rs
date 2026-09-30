//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture routine namespaces, bind catalog dependencies, and recompile changed definitions.

use super::compilation::{self, StoredRoutineCompilationContext};
use std::sync::Arc;
use uqa_sql::{
    ast::{CreateFunction, FunctionBody},
    binding::stored_columns::StoredSourceCatalog,
    routines::{
        compilation::compile_function_body,
        dependencies::{self, RoutineCompilationMode},
        regclass::{self, RoutineRegclassCatalog},
        CompiledFunctionBody, RoutineBody,
    },
    SQLError,
};

pub struct RoutineDefinitionContext<'a> {
    pub compilation: StoredRoutineCompilationContext<'a>,
    pub sources: &'a dyn StoredSourceCatalog,
    pub regclasses: &'a dyn RoutineRegclassCatalog,
}

/// A routine definition bound to the catalog.
pub struct BoundRoutine {
    /// The body as the catalog keeps it.
    pub body: RoutineBody,
    /// The compilation that validated a source body when the routine was defined.
    pub validated: Option<CompiledFunctionBody>,
    /// Whether binding changed the stored definition.
    pub changed: bool,
}

pub fn compile_catalog_bound_routine(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<BoundRoutine, SQLError> {
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
    let mut compiled = compile_routine_for_mode(&context.compilation, def, mode)?;
    let body_changed = with_creation_search_path(context, def, |def| {
        dependencies::bind_sql_standard_body_routines(&context.compilation.analysis, def, mode)
    })? | bind_routine_regclass_constants(context, def)?;
    changed |= body_changed;
    if body_changed {
        compiled = compile_routine_for_mode(&context.compilation, def, mode)?;
    }
    if !matches!(def.body, FunctionBody::Statements(_)) {
        return Ok(BoundRoutine {
            body: RoutineBody::Source,
            validated: compiled,
            changed,
        });
    }
    let compiled = compiled.ok_or_else(|| {
        SQLError::Internal(format!(
            "SQL-standard body of routine `{}` was not compiled",
            def.name
        ))
    })?;
    Ok(BoundRoutine {
        body: RoutineBody::Bound(Arc::new(compiled)),
        validated: None,
        changed,
    })
}

/// Compile the body when the mode needs it: a SQL-standard body always, and a source body only to validate a new definition. The sessions that run a stored source body compile it.
fn compile_routine_for_mode(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<Option<CompiledFunctionBody>, SQLError> {
    match (mode, &def.body) {
        (RoutineCompilationMode::Definition, _) => {
            compile_function_body(&context.analysis, def).map(Some)
        }
        (RoutineCompilationMode::Persisted, FunctionBody::Statements(_)) => {
            compilation::compile_persisted_sql_function(context, def).map(Some)
        }
        (RoutineCompilationMode::Persisted, FunctionBody::Source(_)) => Ok(None),
    }
}

fn bind_routine_definition_dependencies(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    with_creation_search_path(context, def, |def| {
        dependencies::bind_routine_definition_dependencies(
            &context.compilation.analysis,
            context.sources,
            def,
            mode,
        )
    })
}

/// Resolve names in stored routine syntax with the search path captured when the routine was created.
fn with_creation_search_path<T>(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    bind: impl FnOnce(&mut CreateFunction) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
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
