//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric syntax binding retains new signature names and deferred errors under its enclosing expression lease.

use super::numeric_operator_types_with_control;
use crate::{
    ast::{FunctionBinding, FunctionResolutionError, NumericOperator, OperatorResolutionError},
    type_resolution::call,
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

pub(in crate::type_resolution) fn bind_call_in_place_with_control(
    operator: NumericOperator,
    binding: &mut FunctionBinding,
    arguments: &[ScalarExpr],
    memory: &mut Option<MemoryReservation>,
    infer: &mut call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    call::check_memory(memory.as_ref(), control)?;
    if !binding.argument_types.is_empty() || binding.resolution_error.is_some() {
        return Ok(());
    }
    let mut types = ProductionVec::new(*control);
    for argument in arguments {
        control.check()?;
        let ty = match call::infer_with_control(argument, infer, control) {
            Ok(ty) => ty,
            Err(error) if is_resource_error(&error) => return Err(error),
            Err(_) => {
                control.check()?;
                return Ok(());
            }
        };
        let ty = match ty {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                control.finish(Some(ty), memory)?
            }
            None => control.finish(None, control.empty_reservation())?,
        };
        types.push_produced(ty)?;
    }
    // A schema-free pass cannot choose a signature for a still-unresolved column, routine or subquery. Parser unknown constants can be resolved.
    if arguments.iter().zip(types.iter()).any(|(argument, ty)| {
        ty.is_none()
            && !matches!(
                argument,
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
            )
    }) {
        return Ok(());
    }
    match numeric_operator_types_with_control(operator, &types, control) {
        Ok(selected) => {
            let mut names = ProductionVec::new(*control);
            names.reserve(selected.arguments.len())?;
            for ty in &selected.arguments {
                names.push_produced(ty.sql_name_with_control(control)?)?;
            }
            let names = names.finish()?;
            let (names, extra) = names.into_parts();
            *memory = control.combine(memory.take(), extra);
            binding.argument_types = names;
        }
        Err(error) if is_resource_error(&error) => return Err(error),
        Err(error) => {
            let error = retained_error(&error, control)?;
            let (error, extra) = error.into_parts();
            *memory = control.combine(memory.take(), extra);
            binding.resolution_error = Some(error);
        }
    }
    Ok(())
}

fn is_resource_error(error: &SQLError) -> bool {
    matches!(error.sqlstate(), Some("53200" | "57014"))
}

fn retained_error(
    error: &SQLError,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionResolutionError>, SQLError> {
    let sqlstate = control.copy_text(error.sqlstate().unwrap_or("XX000"))?;
    let message = control.format(format_args!("{error}"))?;
    let box_memory = control.reserve(size_of::<OperatorResolutionError>())?;
    // Every constructor has admitted its allocation before the values leave their guards. The completed error owns all three leases before its final cancellation check.
    let (sqlstate, state_memory) = sqlstate.into_parts();
    let (message, message_memory) = message.into_parts();
    let memory = control.combine(control.combine(state_memory, message_memory), box_memory);
    Ok(control.finish(
        FunctionResolutionError::Operator(Box::new(OperatorResolutionError { sqlstate, message })),
        memory,
    )?)
}

#[cfg(test)]
mod tests;
