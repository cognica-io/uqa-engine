//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parameter namespaces use the declaration's written naming rules without copying intermediate name/type vectors.

use super::{
    sql_body_parameters, CreateFunction, FunctionBody, RoutineParameterScope, SQLError, SQLParam,
};
use uqa_core::{memory::ProductionControl, RelationIdentity};

pub fn sql_body_parameter_scope_with_control(
    definition: &CreateFunction,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<RoutineParameterScope, SQLError> {
    control.check()?;
    let reference =
        RelationIdentity::try_borrowed_reference(&definition.name).map_err(|error| {
            SQLError::Internal(format!(
                "invalid routine name `{}`: {error}",
                definition.name
            ))
        })?;
    let name = control.format(format_args!("{}", reference.name))?;
    match definition.body {
        FunctionBody::Statements(_) => RoutineParameterScope::new_with_control(
            &name,
            definition
                .sql_body_parameters()
                .map(|parameter| parameter.name)
                .zip(params.iter().map(SQLParam::declared_scalar_type)),
            control,
        ),
        FunctionBody::Source(_) => RoutineParameterScope::new_with_control(
            &name,
            sql_body_parameters(definition)
                .map(|parameter| parameter.name.as_str())
                .zip(params.iter().map(SQLParam::declared_scalar_type)),
            control,
        ),
    }
}
