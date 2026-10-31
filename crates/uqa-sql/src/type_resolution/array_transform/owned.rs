//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array-transform binding selects the same visible overload before permuting independent argument owners.

use super::{
    argument_types_with_parameters, is_function, preparation, production, FunctionBinding,
    FunctionTypeResolver, InferType, Produced, ProductionControl, ProductionVec, SQLError,
    SQLParam,
};
use crate::{plan::OwnedScalarExpr, type_resolution::OwnedBindingCall};

pub(in crate::type_resolution) fn bind_owned_call(
    call: OwnedBindingCall,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    control.check()?;
    if call.binding().is_some() || !is_function(call.name()) {
        return Ok(call);
    }
    let selected = select_user(&call, params, infer, resolver, control);
    match selected {
        Ok(Some(selected)) => return call.rebind(selected, control),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Ok(None) | Err(_) => {}
    }
    let Some(preparation::Prepared {
        mut positions,
        mut cast_names,
        selected,
    }) = preparation::prepare(call.name(), call.arguments(), infer, control)?
    else {
        return Ok(call);
    };
    let mut call = call;
    for destination in 0..call.arguments().len() {
        let source = positions
            .iter()
            .position(|position| *position == destination)
            .expect("validated positions fill each slot");
        call = call.swap_arguments(destination, source, control)?;
        positions.as_mut_slice().swap(destination, source);
    }
    drop(positions);
    for (position, name) in cast_names
        .iter_mut()
        .enumerate()
        .take(call.arguments().len())
    {
        call = call.map_argument(position, control, |argument| {
            let argument = argument.into_call_argument(control)?;
            match name.take() {
                Some(name) => Ok(OwnedScalarExpr::cast(argument, name, true, control)?),
                None => Ok(argument),
            }
        })?;
    }
    match selected {
        Some(selected) => call.rebind(selected, control),
        None => Ok(call),
    }
}

fn select_user(
    call: &OwnedBindingCall,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<FunctionBinding>>, SQLError> {
    let arguments = crate::ir::scalar_call_arguments_with_control(call.arguments(), control)?;
    let variadic = arguments.iter().any(|argument| argument.explicit_variadic);
    let mut types = ProductionVec::new(*control);
    types.reserve(arguments.len())?;
    for argument in arguments.iter() {
        let ty = super::super::call::infer_with_control(argument.value, infer, control)?;
        let (ty, memory) = match ty {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        types.push_produced(control.finish(ty, memory)?)?;
    }
    let types = types.finish()?;
    let types =
        argument_types_with_parameters(call.name(), call.arguments(), &types, params, control)?;
    match production::select_overload_with_control(
        call.name(),
        None,
        call.arguments(),
        &types,
        variadic,
        resolver,
        control,
    )? {
        production::OwnedSelection::User(selected) => {
            Ok(Some(selected.binding.clone_with_control(control)?))
        }
        production::OwnedSelection::Builtin(_) => Ok(None),
    }
}
