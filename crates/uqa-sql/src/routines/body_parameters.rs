//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The parameters a SQL routine's body names, by name or by position, as `PostgreSQL`'s parser resolves them in each statement of the body.

use super::{compilation::RoutineCompilationContext, routine_local_name};
use crate::{
    ast::{CreateFunction, FunctionParam, FunctionParamMode},
    binding::{bind_routine_parameter_references, RoutineParameterScope},
    plan::UnifiedPlan,
    SQLError, SQLParam,
};

/// The parameters a SQL body can name: the routine's input parameters, which `get_func_input_arg_names` lists.
#[must_use]
pub fn sql_body_parameters(def: &CreateFunction) -> Vec<&FunctionParam> {
    def.params
        .iter()
        .filter(|parameter| is_sql_body_parameter(parameter))
        .collect()
}

/// Whether the body can name `parameter`: an input parameter. A procedure's output parameter takes a placeholder in `CALL` but is not a parameter of its body.
#[must_use]
pub const fn is_sql_body_parameter(parameter: &FunctionParam) -> bool {
    matches!(
        parameter.mode,
        FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
    )
}

/// The scope of the parameters a SQL body names, typed as `params` types them.
pub fn sql_body_parameter_scope(
    def: &CreateFunction,
    params: &[SQLParam],
) -> Result<RoutineParameterScope, SQLError> {
    Ok(RoutineParameterScope::new(
        &routine_local_name(&def.name)?,
        sql_body_parameters(def)
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        params
            .iter()
            .map(|param| param.declared_scalar_type().cloned())
            .collect(),
    ))
}

/// Resolve the parameter references of one statement of a body given as a string, against the catalog as it stands when the statement is analyzed, as `PostgreSQL` analyzes each statement of such a body just before it runs it.
pub fn resolve_sql_body_parameters(
    context: &RoutineCompilationContext<'_>,
    scope: &RoutineParameterScope,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let snapshot = context.catalog.binding_snapshot()?;
    bind_routine_parameter_references(context.routines, plan, params, &snapshot.context(), scope)
}
