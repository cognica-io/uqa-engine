//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile stored routine bodies inside their recorded creation namespace.

use uqa_sql::{
    ast::{CreateFunction, FunctionBody},
    routines::{
        compilation::{self as analysis, RoutineCompilationContext},
        merge_columns::routine_has_removed_merge_target,
        CompiledFunctionBody,
    },
    SQLError,
};

pub trait RoutineCompilationSession {
    fn routine_search_path(&self) -> Vec<String>;
    fn replace_routine_search_path(&self, path: Vec<String>) -> Vec<String>;
    fn restore_routine_search_path(&self, path: Vec<String>);
}
#[derive(Clone, Copy)]
pub struct StoredRoutineCompilationContext<'a> {
    pub analysis: RoutineCompilationContext<'a>,
    pub session: &'a dyn RoutineCompilationSession,
}

#[derive(Clone, Copy)]
enum StoredRoutineCompilation {
    Executable,
    Dependencies,
}

pub fn compile_persisted_sql_function(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_stored_function(context, def, StoredRoutineCompilation::Executable)
}
pub fn stored_merge_dependency_body(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<Option<CompiledFunctionBody>, SQLError> {
    if routine_has_removed_merge_target(context.analysis.merge, def)? {
        compile_stored_function(context, def, StoredRoutineCompilation::Dependencies).map(Some)
    } else {
        Ok(None)
    }
}

fn compile_stored_function(
    context: &StoredRoutineCompilationContext<'_>,
    def: &CreateFunction,
    mode: StoredRoutineCompilation,
) -> Result<CompiledFunctionBody, SQLError> {
    if !matches!(def.body, FunctionBody::Statements(_)) || def.creation_search_path.is_empty() {
        return compile_current_function(&context.analysis, def, mode);
    }
    let previous = context
        .session
        .replace_routine_search_path(def.creation_search_path.clone());
    let compiled = compile_current_function(&context.analysis, def, mode);
    context.session.restore_routine_search_path(previous);
    compiled
}

fn compile_current_function(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    mode: StoredRoutineCompilation,
) -> Result<CompiledFunctionBody, SQLError> {
    match mode {
        StoredRoutineCompilation::Executable => {
            analysis::compile_persisted_function_body(context, def)
        }
        StoredRoutineCompilation::Dependencies => {
            analysis::compile_persisted_function_dependencies(context, def)
        }
    }
}
