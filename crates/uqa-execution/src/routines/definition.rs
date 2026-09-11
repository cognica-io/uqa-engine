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
        compilation::compile_function_body,
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

pub fn compile_catalog_bound_routine(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<(CompiledFunctionBody, bool), SQLError> {
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
        compiled = compile_routine_for_mode(&context.compilation, def, mode)?;
    }
    Ok((compiled, changed))
}

fn compile_routine_for_mode(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<CompiledFunctionBody, SQLError> {
    match mode {
        RoutineCompilationMode::Definition => compile_function_body(&context.analysis, def),
        RoutineCompilationMode::Persisted => {
            compilation::compile_persisted_sql_function(context, def)
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
