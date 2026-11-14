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

/// Build the original ordered identity signature through a retained input's work channel. Output-only parameters still consume work without entering the identity.
pub async fn routine_signature_types_from_input(
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<Produced<Vec<String>>, crate::type_resolution::RoutineSignatureInputError> {
    control.check()?;
    input.checkpoint().await?;
    control.check()?;
    let mut signature = uqa_core::memory::ProductionVec::new(*control);
    for parameter in &definition.params {
        control.check()?;
        input.checkpoint().await?;
        control.check()?;
        if CreateFunction::is_identity_param(parameter) {
            signature.push_produced(
                crate::type_resolution::canonical_routine_type_name_from_input(
                    &parameter.type_name,
                    control,
                    input,
                )
                .await?,
            )?;
        }
    }
    Ok(signature.finish()?)
}

#[cfg(test)]
mod tests;
