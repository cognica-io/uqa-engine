//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Overload signatures retain decoded argument observations and selected type metadata while producing their owned fields.

use super::{
    FunctionCallArgumentSignature, FunctionTypeResolver, SQLError, SQLParam, ScalarExpr,
    ScalarTypeSchema,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn function_call_argument_signature_with_control(
    arguments: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionCallArgumentSignature>, SQLError> {
    let arguments = crate::ir::scalar_call_arguments_with_control(arguments, control)?;
    let mut names = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    names.reserve(arguments.len())?;
    types.reserve(arguments.len())?;
    let mut variadic = false;
    for argument in arguments.iter() {
        control.check()?;
        variadic |= argument.explicit_variadic;
        let (name, memory) = match argument.name {
            Some(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                (Some(name), memory)
            }
            None => (None, control.empty_reservation()),
        };
        names.push_produced(control.finish(name, memory)?)?;
        let inferred = super::common_context_expression_type_with_control(
            argument.value,
            schema,
            params,
            resolver,
            control,
        )?;
        let effective = super::effective_overload_argument_type_ref_with_params(
            argument.value,
            inferred.as_deref(),
            params,
        );
        let (ty, memory) = match effective {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        types.push_produced(control.finish(ty, memory)?)?;
    }
    let names = names.finish()?;
    let types = types.finish()?;
    let (names, name_memory) = names.into_parts();
    let (types, type_memory) = types.into_parts();
    Ok(control.finish(
        (names, types, variadic),
        control.combine(name_memory, type_memory),
    )?)
}
