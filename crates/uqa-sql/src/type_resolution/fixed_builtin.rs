//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One registry and binding path for implemented fixed-signature `PostgreSQL` built-ins.

mod registry;
mod selected;
mod selection;
mod standard;

use crate::ast::{ColumnType, FunctionBinding, FunctionDispatch};
use crate::{SQLError, SQLParam};
use uqa_core::Value;

use crate::{scalar_call_arguments, schema::ScalarTypeSchema, ScalarExpr};

use super::common::base_type;
use super::functions::{named_argument, named_argument_value};
use super::{
    builtin_binding_matches, canonical_column_type_name, canonical_routine_type_name,
    match_builtin_function_overload, scalar_type_inner, BuiltinFunctionOverload,
    FunctionTypeResolver, ResolvedFunctionOverload,
};

#[doc(hidden)]
#[must_use]
pub fn is_function(name: &str) -> bool {
    registry::lookup(name).is_some()
}

/// Fixed-signature call metadata needed by generated-column binding without exposing the built-in registry itself.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFixedBuiltinCall {
    pub selected: ResolvedFunctionOverload,
    pub builtin_argument_positions: Option<Vec<usize>>,
    pub builtin_non_immutable: bool,
}

/// Resolve an implemented fixed-signature built-in together with visible SQL routine overloads. `None` means `name` is outside the fixed registry.
#[doc(hidden)]
pub fn resolve_fixed_builtin_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFixedBuiltinCall>, SQLError> {
    let Some(builtins) = overloads(name) else {
        return Ok(None);
    };
    let selected = resolve_overload(
        name,
        binding,
        argument_names,
        argument_types,
        explicit_variadic,
        resolver,
    )?;
    let (builtin_argument_positions, builtin_non_immutable) = if selected.binding.builtin {
        let matched = builtins
            .iter()
            .find(|overload| builtin_binding_matches(overload, &selected.binding))
            .cloned()
            .and_then(|overload| {
                match_builtin_function_overload(overload, argument_names, argument_types)
            })
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved fixed built-in `{}` lost its catalog signature",
                    selected.binding.name
                ))
            })?;
        (
            Some(matched.argument_positions),
            builtin_binding_is_non_immutable(&selected.binding),
        )
    } else {
        (None, false)
    };
    Ok(Some(ResolvedFixedBuiltinCall {
        selected,
        builtin_argument_positions,
        builtin_non_immutable,
    }))
}

/// Return the catalog result type encoded by a stable fixed built-in binding.
#[doc(hidden)]
#[must_use]
pub fn fixed_builtin_return_type(binding: &FunctionBinding) -> Option<ColumnType> {
    registry::bound_signature(
        binding,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary fixed binding lookup cannot be cancelled or limited")
    .map(|signature| signature.return_type.clone())
}

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let argument_names = argument_names(args);
    let argument_types = effective_argument_types(args, argument_types, params);
    resolve_overload(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        resolver,
    )
    .map(|overload| Some(overload.return_type))
}

pub(super) fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedFunctionOverload, SQLError> {
    crate::expr::validate_named_argument_order(argument_names.iter().map(Option::as_deref))?;
    if let Some(resolver) = resolver {
        let builtins = overloads(name).ok_or_else(|| {
            super::function_resolution_error(
                "42883",
                name,
                argument_names,
                argument_types,
                "does not exist",
            )
        })?;
        if let Some(selected) = resolver.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            &builtins,
        )? {
            return Ok(selected);
        }
    }
    selection::resolve_overload_with_control(
        name,
        binding,
        argument_names,
        argument_types,
        explicit_variadic,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|selected| {
        selected
            .into_uncontrolled()
            .expect("ordinary fixed selection has no reservation")
    })
}

pub(super) fn selected_argument_targets(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Option<Vec<Option<ColumnType>>> {
    let argument_names = vec![None; argument_types.len()];
    let selected =
        resolve_overload(name, None, &argument_names, argument_types, false, None).ok()?;
    let declared = selected
        .binding
        .argument_types
        .iter()
        .map(|ty| ColumnType::from_sql_name(ty).ok())
        .take(argument_types.len())
        .collect::<Vec<_>>();
    (declared.len() == argument_types.len()).then_some(declared)
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_none() && resolver.is_some_and(|resolver| resolver.has_untyped_function(&name)) {
        return name;
    }
    let Some((registered_name, _)) = registry::lookup(&name) else {
        return name;
    };
    let Ok(call_arguments) = scalar_call_arguments(args) else {
        return name;
    };
    let explicit_variadic = call_arguments
        .iter()
        .any(|argument| argument.explicit_variadic);
    let Ok(argument_types) = call_arguments
        .iter()
        .map(|argument| scalar_type_inner(argument.value, schema, params, resolver))
        .collect::<Result<Vec<_>, _>>()
    else {
        return name;
    };
    let names = argument_names(args);
    let effective_types = effective_argument_types(args, &argument_types, params);
    if resolver.is_none()
        && !(explicit_variadic && names.iter().any(Option::is_some))
        && binding.as_mut().is_some_and(|binding| {
            registry::lookup(&binding.name)
                .is_some_and(|(selected_name, _)| selected_name == registered_name)
                && bind_selected_call(binding, args, &names, &argument_types, &effective_types)
        })
    {
        return name;
    }
    let builtins = overloads(&name).expect("fixed registry membership was checked");
    let selected = resolve_overload(
        &name,
        binding.as_ref(),
        &names,
        &effective_types,
        explicit_variadic,
        resolver,
    );
    let mut selected = match selected {
        Ok(selected) => selected,
        Err(error) if error.sqlstate() == Some("42883") => {
            let signature = unresolved_call_signature(&name, &names, &effective_types);
            *binding = Some(FunctionBinding::undefined_function(name.clone(), signature));
            return name;
        }
        Err(_) => return name,
    };
    if !selected.binding.builtin {
        *binding = Some(selected.binding);
        return name;
    }
    let Some(matched) = builtins
        .iter()
        .find(|overload| builtin_binding_matches(overload, &selected.binding))
        .cloned()
        .and_then(|overload| match_builtin_function_overload(overload, &names, &effective_types))
    else {
        return name;
    };
    let overload = matched.overload;
    if !reorder_arguments(
        args,
        &matched.argument_positions,
        overload.argument_types.len(),
        &selected.binding.name,
    ) {
        return name;
    }
    coerce_arguments(args, &overload.argument_types, schema, params, resolver);
    selected.binding.dispatch = runtime_dispatch(&selected.binding);
    *binding = Some(selected.binding);
    name
}

/// Already selected generated-column bindings need structural argument matching and coercion, not candidate construction or ranking. Invalid bindings continue through the existing diagnostic path.
fn bind_selected_call(
    binding: &mut FunctionBinding,
    args: &mut Vec<ScalarExpr>,
    names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    effective_types: &[Option<ColumnType>],
) -> bool {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let call = selected::SelectedCall::take(binding, args);
    let call = control
        .finish(call, None)
        .expect("uncontrolled input owner");
    let (matched, call) =
        selected::bind_call_with_control(call, names, argument_types, effective_types, &control)
            .expect("ordinary fixed binding constructors cannot be cancelled or limited");
    let call = call.into_uncontrolled().expect("uncontrolled output owner");
    *binding = call.binding;
    *args = call.arguments;
    matched
}

fn coerce_arguments(
    args: &mut [ScalarExpr],
    declared: &[ColumnType],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    for (argument, declared) in args.iter_mut().zip(declared) {
        let actual = scalar_type_inner(argument, schema, params, resolver)
            .ok()
            .flatten();
        if requires_cast(argument, actual.as_ref(), declared) {
            *argument = ScalarExpr::Cast {
                expr: Box::new(std::mem::replace(
                    argument,
                    ScalarExpr::Literal(Value::Null),
                )),
                ty: declared.sql_name(),
            };
        }
    }
}

fn requires_cast(
    argument: &ScalarExpr,
    actual: Option<&ColumnType>,
    declared: &ColumnType,
) -> bool {
    if matches!(
        argument,
        ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
    ) {
        return true;
    }
    actual.is_none_or(|actual| {
        canonical_column_type_name(base_type(actual))
            != canonical_routine_type_name(&declared.sql_name())
    })
}

pub(crate) fn runtime_dispatch(binding: &FunctionBinding) -> Option<FunctionDispatch> {
    let local = binding.name.rsplit('.').next()?;
    let arguments = binding.argument_types.as_slice();
    Some(match (local, arguments) {
        ("to_bin", [ty]) if ty == "integer" => FunctionDispatch::ToBinInt4,
        ("to_bin", [ty]) if ty == "bigint" => FunctionDispatch::ToBinInt8,
        ("to_hex", [ty]) if ty == "integer" => FunctionDispatch::ToHexInt4,
        ("to_hex", [ty]) if ty == "bigint" => FunctionDispatch::ToHexInt8,
        ("to_oct", [ty]) if ty == "integer" => FunctionDispatch::ToOctInt4,
        ("to_oct", [ty]) if ty == "bigint" => FunctionDispatch::ToOctInt8,
        ("random", [left, right]) if left == "integer" && right == "integer" => {
            FunctionDispatch::RandomInt4Range
        }
        ("random", [left, right]) if left == "bigint" && right == "bigint" => {
            FunctionDispatch::RandomInt8Range
        }
        ("random", [left, right]) if left == "numeric" && right == "numeric" => {
            FunctionDispatch::RandomNumericRange
        }
        _ => return None,
    })
}

fn builtin_binding_is_non_immutable(binding: &FunctionBinding) -> bool {
    crate::schema::generated::eligibility::fixed_builtin_is_non_immutable(
        binding.name.rsplit('.').next().unwrap_or_default(),
    )
}

fn default_argument(name: &str, position: usize) -> Option<ScalarExpr> {
    matches!(
        (name.rsplit('.').next(), position),
        (Some("json_strip_nulls" | "jsonb_strip_nulls"), 1)
    )
    .then_some(ScalarExpr::Literal(Value::Bool(false)))
}

fn reorder_arguments(
    args: &mut Vec<ScalarExpr>,
    argument_positions: &[usize],
    parameter_count: usize,
    binding_name: &str,
) -> bool {
    if args.len() != argument_positions.len() {
        return false;
    }
    let mut supplied = vec![false; parameter_count];
    for &position in argument_positions {
        let Some(slot) = supplied.get_mut(position) else {
            return false;
        };
        if std::mem::replace(slot, true) {
            return false;
        }
    }
    let mut reordered = (0..parameter_count)
        .map(|position| default_argument(binding_name, position))
        .collect::<Vec<_>>();
    if supplied
        .iter()
        .zip(&reordered)
        .any(|(supplied, default)| !supplied && default.is_none())
    {
        return false;
    }
    for (argument, &position) in std::mem::take(args).into_iter().zip(argument_positions) {
        reordered[position] = Some(named_argument_value_owned(argument));
    }
    *args = reordered
        .into_iter()
        .map(|argument| argument.expect("every fixed built-in argument was prevalidated"))
        .collect();
    true
}

fn argument_names(args: &[ScalarExpr]) -> Vec<Option<String>> {
    args.iter()
        .map(|argument| named_argument(argument).0)
        .collect()
}

fn effective_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    params: &[SQLParam],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument = named_argument_value(argument);
            super::effective_overload_argument_type_with_params(
                argument,
                argument_type.clone(),
                params,
            )
        })
        .collect()
}

fn unresolved_call_signature(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> String {
    let arguments = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |name| {
                    format!("{name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({arguments})")
}

fn named_argument_value_owned(expression: ScalarExpr) -> ScalarExpr {
    if matches!(
        &expression,
        ScalarExpr::Func { binding, args, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(FunctionDispatch::NamedArgument)
                && args.len() == 2
    ) {
        let ScalarExpr::Func { mut args, .. } = expression else {
            unreachable!();
        };
        return args.pop().expect("named argument value follows its name");
    }
    expression
}

fn overloads(name: &str) -> Option<Vec<BuiltinFunctionOverload>> {
    let (name, signatures) = registry::lookup(name)?;
    Some(
        signatures
            .iter()
            .map(|signature| BuiltinFunctionOverload {
                name: format!("pg_catalog.{name}"),
                argument_names: if signature.argument_names.is_empty() {
                    vec![None; signature.argument_types.len()]
                } else {
                    signature
                        .argument_names
                        .iter()
                        .map(|name| Some((*name).into()))
                        .collect()
                },
                argument_types: signature.argument_types.to_vec(),
                default_arguments: signature.default_arguments,
                return_type: signature.return_type.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod privilege_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_gate_accepts_only_supported_builtin_names() {
        assert!(is_function("PG_CATALOG.MD5"));
        assert!(!is_function("public.md5"));
        assert!(!is_function("not_a_builtin"));
    }

    #[test]
    fn failed_argument_reordering_preserves_original_arguments() {
        let original = vec![ScalarExpr::Literal(Value::Int(7))];
        let mut args = original.clone();

        assert!(!reorder_arguments(&mut args, &[1], 2, "pg_catalog.random"));
        assert_eq!(args, original);
    }
}

#[cfg(test)]
mod selected_tests;
