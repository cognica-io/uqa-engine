//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected routine names and return categories borrow declarations until their destination is admitted.

use super::{CreateFunction, FunctionReturns};
use crate::{type_resolution::canonical_routine_type_name_with_control, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub fn routine_local_name_with_control(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let reference = match RelationIdentity::try_borrowed_reference(name) {
        Ok(reference) => reference,
        Err(error) => {
            let message = control.format(format_args!("invalid routine name `{name}`: {error}"))?;
            return Err(SQLError::Internal(message.into_parts().0));
        }
    };
    Ok(control.format(format_args!("{}", reference.name))?)
}

pub fn routine_returns_anonymous_record_with_control(
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let mut outputs = definition.output_params();
    let name = match (outputs.next(), outputs.next(), &definition.returns) {
        (Some(output), None, _) => &output.type_name,
        (
            None,
            None,
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name },
        ) => type_name,
        _ => return Ok(false),
    };
    Ok(canonical_routine_type_name_with_control(name, control)?.as_str() == "record")
}
