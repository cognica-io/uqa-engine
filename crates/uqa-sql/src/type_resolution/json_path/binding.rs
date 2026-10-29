//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalize selected JSON path calls to their declared array argument while retaining every input lease.

use super::{
    resolve_call, ColumnType, FunctionBinding, FunctionDispatch, FunctionTypeResolver,
    JsonPathFunction, Produced, ProductionControl, RoutineCallDescriptor, SQLError,
};
use crate::type_resolution::call::{self, BindingCall, InferType};
use crate::{SQLParam, ScalarExpr};
use uqa_core::{
    memory::{MemoryReservation, ProductionVec},
    Value,
};

pub(in crate::type_resolution) fn bind_call(
    call: &mut BindingCall,
    memory: &mut Option<MemoryReservation>,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    call::check_memory(memory.as_ref(), control)?;
    if JsonPathFunction::lookup(&call.name).is_none()
        || matches!(
            call.binding.as_ref().and_then(|binding| binding.dispatch),
            Some(FunctionDispatch::JsonPath { .. })
        )
        || call.binding.is_none()
            && resolver.is_some_and(|resolver| resolver.has_untyped_function(&call.name))
    {
        return Ok(());
    }
    let Signature {
        names,
        original,
        effective,
        explicit_variadic,
    } = signature(&call.arguments, params, infer, control)?;
    let selected = match resolve_call(
        &call.name,
        call.binding.as_ref(),
        RoutineCallDescriptor {
            argument_names: &names,
            argument_types: &effective,
            explicit_variadic,
        },
        resolver,
        control,
    ) {
        Ok(Some(selected)) => selected,
        Ok(None) => return Ok(()),
        Err(error) if error.sqlstate() == Some("42883") => {
            call::retain_undefined_call(call, memory, &names, &effective, control)?;
            return Ok(());
        }
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Err(_) => return Ok(()),
    };
    let (selected, extra) = selected.into_parts();
    *memory = control.combine(memory.take(), extra);
    if !selected.binding.builtin {
        crate::ast::FunctionBinding::rebind(&mut call.binding, selected.binding);
        return Ok(());
    }
    let mut binding = selected.binding;
    normalize(
        &mut binding,
        &mut call.arguments,
        &original,
        explicit_variadic,
        memory,
        control,
    )?;
    crate::ast::FunctionBinding::rebind(&mut call.binding, binding);
    Ok(())
}

pub(super) struct Signature {
    pub(super) names: Produced<Vec<Option<String>>>,
    pub(super) original: Produced<Vec<Option<ColumnType>>>,
    pub(super) effective: Produced<Vec<Option<ColumnType>>>,
    pub(super) explicit_variadic: bool,
}

pub(super) fn signature(
    arguments: &[ScalarExpr],
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Signature, SQLError> {
    let decoded = crate::ir::scalar_call_arguments_with_control(arguments, control)?;
    let explicit_variadic = decoded.iter().any(|argument| argument.explicit_variadic);
    let mut names = ProductionVec::new(*control);
    let mut original = ProductionVec::new(*control);
    let mut effective = ProductionVec::new(*control);
    names.reserve(decoded.len())?;
    original.reserve(decoded.len())?;
    effective.reserve(decoded.len())?;
    for argument in decoded.iter() {
        let name = argument
            .name
            .map(|name| control.copy_text(name))
            .transpose()?;
        let (name, extra) = name.map_or((None, control.empty_reservation()), |name| {
            let (name, extra) = name.into_parts();
            (Some(name), extra)
        });
        names.push_produced(control.finish(name, extra)?)?;
        let ty = call::infer_with_control(argument.value, infer, control)?;
        let effective_type =
            crate::type_resolution::common::effective_overload_argument_type_ref_with_params(
                argument.value,
                ty.as_deref(),
                params,
            );
        push_type(
            &mut effective,
            effective_type
                .map(|ty| ty.clone_with_control(control))
                .transpose()?,
            control,
        )?;
        push_type(&mut original, ty, control)?;
    }
    let names = names.finish()?;
    let original = original.finish()?;
    let effective = effective.finish()?;
    drop(decoded);
    Ok(Signature {
        names,
        original,
        effective,
        explicit_variadic,
    })
}

pub(super) fn invocation(
    binding: &FunctionBinding,
    argument_count: usize,
    original_count: usize,
    explicit_variadic: bool,
) -> Result<&crate::ast::RoutineInvocationBinding, SQLError> {
    let invocation = binding
        .invocation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("JSON path call lost its variadic contract".into()))?;
    if original_count != argument_count
        || invocation.argument_targets.len() != argument_count
        || invocation.argument_positions.len() != argument_count
        || invocation
            .argument_positions
            .iter()
            .filter(|&&position| position == 0)
            .count()
            != 1
        || invocation
            .argument_positions
            .iter()
            .any(|&position| position > 1)
        || argument_count < 2
        || explicit_variadic && argument_count != 2
    {
        return Err(SQLError::Internal(
            "invalid selected JSON path invocation".into(),
        ));
    }
    Ok(invocation)
}

fn normalize(
    binding: &mut FunctionBinding,
    arguments: &mut Vec<ScalarExpr>,
    original: &[Option<ColumnType>],
    explicit_variadic: bool,
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let entry = JsonPathFunction::lookup(&binding.name).expect("selected JSON path declaration");
    let invocation = invocation(binding, arguments.len(), original.len(), explicit_variadic)?;
    let mut document = None;
    let mut paths = ProductionVec::new(*control);
    paths.reserve(arguments.len().saturating_sub(1))?;
    for (index, argument) in arguments.iter_mut().enumerate() {
        let target =
            ColumnType::from_sql_name_with_control(&invocation.argument_targets[index], control)?;
        let value = crate::scalar_call_argument(argument)?.value;
        let actual = match value {
            ScalarExpr::TypedLiteral {
                bound_type: Some(ty),
                ..
            } => Some(ty),
            _ => original[index].as_ref(),
        };
        let needs_cast = actual != Some(&target)
            || matches!(
                value,
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
            );
        let owned = std::mem::replace(argument, ScalarExpr::Literal(Value::Null));
        let mut value = crate::ir::scalar_call_argument_value_owned(owned)?;
        if needs_cast {
            let (ty, extra) = target.catalog_name_with_control(control)?.into_parts();
            *memory = control.combine(memory.take(), extra);
            let extra = control.reserve(size_of::<ScalarExpr>())?;
            *memory = control.combine(memory.take(), extra);
            value = ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(value),
                ty,
            };
        }
        if invocation.argument_positions[index] == 0 {
            document = Some(value);
        } else {
            // The enclosing expression still owns the moved payload; this buffer admits its slots only.
            paths.push_produced(control.finish(value, control.empty_reservation())?)?;
        }
    }
    let (mut paths, extra) = paths.finish()?.into_parts();
    *memory = control.combine(memory.take(), extra);
    let path = if explicit_variadic {
        paths
            .pop()
            .ok_or_else(|| SQLError::Internal("JSON path array disappeared".into()))?
    } else {
        ScalarExpr::Array(paths)
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(2)?;
    output.push_produced(control.finish(
        document.ok_or_else(|| SQLError::Internal("JSON path input disappeared".into()))?,
        control.empty_reservation(),
    )?)?;
    output.push_produced(control.finish(path, control.empty_reservation())?)?;
    let (output, extra) = output.finish()?.into_parts();
    *memory = control.combine(memory.take(), extra);
    *arguments = output;
    binding.invocation = None;
    binding.dispatch = Some(entry.dispatch());
    control.check()?;
    Ok(())
}

fn push_type(
    output: &mut ProductionVec<'_, Option<ColumnType>>,
    ty: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
        let (ty, memory) = ty.into_parts();
        (Some(ty), memory)
    });
    Ok(output.push_produced(control.finish(ty, memory)?)?)
}

/// Input analysis already selected and coerced every child before moving the parent call.
pub(crate) fn normalize_retained(expression: &mut ScalarExpr) -> Result<(), SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return Ok(());
    };
    if !binding.builtin
        || JsonPathFunction::lookup(&binding.name).is_none()
        || matches!(binding.dispatch, Some(FunctionDispatch::JsonPath { .. }))
    {
        return Ok(());
    }
    let Some(invocation) = &binding.invocation else {
        return Ok(());
    };
    let original = invocation
        .argument_sources
        .iter()
        .map(|ty| {
            ty.as_deref()
                .and_then(|ty| ColumnType::from_sql_name(ty).ok())
        })
        .collect::<Vec<_>>();
    let explicit = matches!(
        invocation.variadic_mode,
        crate::ast::RoutineVariadicMode::Explicit { .. }
    );
    normalize(
        binding,
        args,
        &original,
        explicit,
        &mut None,
        &ProductionControl::uncontrolled(),
    )
}
