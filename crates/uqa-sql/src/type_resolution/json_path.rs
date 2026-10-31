//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed JSON path calls share ordinary routine precedence and bounded builtin matching.

mod binding;
mod owned;
pub(super) use binding::bind_call;
pub(crate) use binding::normalize_retained;
pub(super) use owned::bind_owned_call;
pub(crate) use owned::normalize_retained_owned;

use super::{
    BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload, RoutineCallDescriptor,
};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    catalog::json_path::{JsonPathFunction, JSON_PATH_FUNCTIONS},
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

static DECLARATIONS: std::sync::LazyLock<[BuiltinFunctionOverload; 4]> =
    std::sync::LazyLock::new(|| {
        JSON_PATH_FUNCTIONS.map(|entry| BuiltinFunctionOverload {
            variadic: true,
            name: format!("pg_catalog.{}", entry.name),
            argument_names: vec![Some("from_json".into()), Some("path_elems".into())],
            argument_types: vec![
                entry.input_type(),
                ColumnType::Array(Box::new(ColumnType::Text)),
            ],
            default_arguments: 0,
            return_type: entry.result_type(),
        })
    });

/// Stored native calls from before typed path selection must enter the existing atomic routine-binding migration.
pub(crate) fn requires_binding(
    name: &str,
    binding: Option<&FunctionBinding>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> bool {
    JsonPathFunction::lookup(name).is_some()
        && !(binding.is_none()
            && resolver.is_some_and(|resolver| resolver.has_untyped_function(name)))
        && binding.is_none_or(|binding| {
            binding.builtin && binding.dispatch.is_none() && binding.resolution_error.is_none()
        })
}

pub(crate) fn resolve_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    mut call: RoutineCallDescriptor<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
    control.check()?;
    if JsonPathFunction::lookup(name).is_none()
        || binding.is_none() && resolver.is_some_and(|resolver| resolver.has_untyped_function(name))
    {
        return Ok(None);
    }
    if matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(FunctionDispatch::JsonPath { .. })
    ) {
        call.explicit_variadic = true;
    }
    if let Some(resolver) = resolver {
        if let Some(selected) = resolver.resolve_function_overload_with_builtins_and_control(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
            &*DECLARATIONS,
            control,
        )? {
            return Ok(Some(selected));
        }
    }
    super::resolve_local_builtin_call_with_control(name, binding, call, &*DECLARATIONS, control)
        .map(Some)
}

#[cfg(test)]
mod tests;

pub(super) fn resolve_type(
    call: super::functions::FunctionTypeCall<'_>,
    arguments: &[crate::ir::ScalarCallArgument<'_>],
    types: &[Option<ColumnType>],
    params: &[crate::SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    use uqa_core::memory::ProductionVec;
    let mut names = ProductionVec::new(*control);
    let mut effective = ProductionVec::new(*control);
    names.reserve(arguments.len())?;
    effective.reserve(arguments.len())?;
    for (argument, ty) in arguments.iter().zip(types) {
        let name = argument
            .name
            .map(|name| control.copy_text(name))
            .transpose()?;
        let (name, memory) = name.map_or((None, control.empty_reservation()), |name| {
            let (name, memory) = name.into_parts();
            (Some(name), memory)
        });
        names.push_produced(control.finish(name, memory)?)?;
        let ty = super::common::effective_overload_argument_type_ref_with_params(
            argument.value,
            ty.as_ref(),
            params,
        );
        let ty = ty.map(|ty| ty.clone_with_control(control)).transpose()?;
        let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
            let (ty, memory) = ty.into_parts();
            (Some(ty), memory)
        });
        effective.push_produced(control.finish(ty, memory)?)?;
    }
    let names = names.finish()?;
    let effective = effective.finish()?;
    resolve_call(
        call.name,
        call.binding,
        RoutineCallDescriptor {
            argument_names: &names,
            argument_types: &effective,
            explicit_variadic: arguments.iter().any(|argument| argument.explicit_variadic),
        },
        resolver,
        control,
    )?
    .map(|selected| {
        selected
            .return_type
            .clone_with_control(control)
            .map_err(Into::into)
    })
    .transpose()
}

pub(super) fn dispatch_type(
    dispatch: FunctionDispatch,
    types: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let [left, right] = types else {
        return Err(SQLError::Internal(
            "JSON extraction requires two operands".into(),
        ));
    };
    if let FunctionDispatch::JsonPath { as_text, jsonb } = dispatch {
        let ty = if as_text {
            ColumnType::Text
        } else if jsonb {
            ColumnType::JsonB
        } else {
            ColumnType::Json
        };
        return Ok(Some(control.finish(ty, control.empty_reservation())?));
    }
    let symbol = dispatch
        .json_extraction_symbol()
        .ok_or_else(|| SQLError::Internal("expected JSON extraction dispatch".into()))?;
    let selected =
        super::operators::named_binary_operator_types_with_control(symbol, *left, *right, control)?;
    Ok(Some(selected[2].clone_with_control(control)?))
}
