//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local fixed calls share admitted signature matching and constructors while the enclosing expression retains every original payload.

use super::{registry, selected, selection};
use crate::type_resolution::{
    call::{self, BindingCall, InferType},
    common,
};
use crate::{
    ast::FunctionBinding, schema::ScalarTypeSchema, ColumnType, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::memory::{
    MemoryReservation, Produced, ProductionControl, ProductionString, ProductionVec,
};

pub(super) fn bind_local_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> String {
    let control = ProductionControl::uncontrolled();
    let mut call = BindingCall {
        name,
        binding: binding.take(),
        arguments: std::mem::take(args),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let mut memory = None;
    let mut infer = |expression: &ScalarExpr| {
        super::super::scalar_type_inner_with_control(expression, schema, params, None, &control)
    };
    bind_call_in_place_with_control(&mut call, &mut memory, params, &mut infer, &control)
        .expect("ordinary fixed binding cannot be cancelled or limited");
    *binding = call.binding;
    *args = call.arguments;
    call.name
}

pub(in crate::type_resolution) fn bind_call_in_place_with_control(
    call: &mut BindingCall,
    memory: &mut Option<MemoryReservation>,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    call::check_memory(memory.as_ref(), control)?;
    let Some((registered, _)) = registry::lookup(&call.name) else {
        return Ok(());
    };
    let signature = match signature(&call.arguments, params, infer, control) {
        Ok(signature) => signature,
        Err(error) => return optional_error(error, control),
    };
    if !(signature.explicit_variadic && signature.names.iter().any(Option::is_some)) {
        if let Some(binding) = call.binding.as_mut().filter(|binding| {
            registry::lookup(&binding.name).is_some_and(|(name, _)| name == registered)
        }) {
            let mut selected = selected::SelectedCall::take(binding, &mut call.arguments);
            let matched = selected::bind_call_in_place_with_control(
                &mut selected,
                memory,
                &signature.names,
                &signature.original,
                &signature.effective,
                control,
            )?;
            *binding = selected.binding;
            call.arguments = selected.arguments;
            if matched {
                return Ok(());
            }
        }
    }
    let selected = match selection::resolve_overload_with_control(
        &call.name,
        call.binding.as_ref(),
        &signature.names,
        &signature.effective,
        signature.explicit_variadic,
        control,
    ) {
        Ok(selected) => selected,
        Err(error) if error.sqlstate() == Some("42883") => {
            let signature = unresolved_call_signature_with_control(
                &call.name,
                &signature.names,
                &signature.effective,
                control,
            )?;
            let name = control.copy_text(&call.name)?;
            let (signature, extra) = signature.into_parts();
            *memory = control.combine(memory.take(), extra);
            let (name, extra) = name.into_parts();
            *memory = control.combine(memory.take(), extra);
            call.binding = Some(FunctionBinding::undefined_function(name, signature));
            return Ok(());
        }
        Err(error) => return optional_error(error, control),
    };
    let (selected, extra) = selected.into_parts();
    *memory = control.combine(memory.take(), extra);
    let mut selected = selected::SelectedCall {
        binding: selected.binding,
        arguments: std::mem::take(&mut call.arguments),
    };
    let matched = selected::bind_call_in_place_with_control(
        &mut selected,
        memory,
        &signature.names,
        &signature.original,
        &signature.effective,
        control,
    )?;
    if matched {
        call.binding = Some(selected.binding);
    }
    call.arguments = selected.arguments;
    control.check()?;
    Ok(())
}

fn optional_error(error: SQLError, control: &ProductionControl<'_>) -> Result<(), SQLError> {
    if control.budget().is_some() && matches!(error.sqlstate(), Some("53200" | "57014")) {
        Err(error)
    } else {
        Ok(())
    }
}

struct Signature {
    names: Produced<Vec<Option<String>>>,
    original: Produced<Vec<Option<ColumnType>>>,
    effective: Produced<Vec<Option<ColumnType>>>,
    explicit_variadic: bool,
}

fn signature(
    arguments: &[ScalarExpr],
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Signature, SQLError> {
    let arguments = crate::ir::scalar_call_arguments_with_control(arguments, control)?;
    let explicit_variadic = arguments.iter().any(|argument| argument.explicit_variadic);
    let mut original = ProductionVec::new(*control);
    original.reserve(arguments.len())?;
    // Inference precedes name construction as in the ordinary binder, including callback errors.
    for argument in arguments.iter() {
        original.push_produced(optional(
            call::infer_with_control(argument.value, infer, control)?,
            control,
        )?)?;
    }
    let original = original.finish()?;
    let mut names = ProductionVec::new(*control);
    let mut effective = ProductionVec::new(*control);
    names.reserve(arguments.len())?;
    effective.reserve(arguments.len())?;
    for (argument, ty) in arguments.iter().zip(original.iter()) {
        names.push_produced(optional(
            argument
                .name
                .map(|name| control.copy_text(name))
                .transpose()?,
            control,
        )?)?;
        let effective_type = common::effective_overload_argument_type_ref_with_params(
            argument.value,
            ty.as_ref(),
            params,
        )
        .map(|ty| ty.clone_with_control(control))
        .transpose()?;
        effective.push_produced(optional(effective_type, control)?)?;
    }
    Ok(Signature {
        names: names.finish()?,
        original,
        effective: effective.finish()?,
        explicit_variadic,
    })
}

fn optional<T>(
    value: Option<Produced<T>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<T>>, SQLError> {
    let Some(value) = value else {
        return Ok(control.finish(None, control.empty_reservation())?);
    };
    let (value, memory) = value.into_parts();
    Ok(control.finish(Some(value), memory)?)
}

pub(super) fn unresolved_call_signature_with_control(
    name: &str,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut signature = ProductionString::new(*control);
    signature.push_str(name)?;
    signature.push('(')?;
    for (position, (name, ty)) in names.iter().zip(types).enumerate() {
        if position > 0 {
            signature.push_str(", ")?;
        }
        if let Some(name) = name {
            signature.push_str(name)?;
            signature.push_str(" => ")?;
        }
        if let Some(ty) = ty {
            signature.push_str(&ty.regtype_name_with_control(control)?)?;
        } else {
            signature.push_str("unknown")?;
        }
    }
    signature.push(')')?;
    Ok(signature.finish()?)
}

#[cfg(test)]
mod tests;
