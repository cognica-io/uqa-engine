//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `gamma(float8)` and `lgamma(float8)`.

use super::common::base_type;
use super::functions::named_argument_value;
use super::{
    scalar_type_inner, BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload,
};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

pub type ResolvedGammaOverload = ResolvedFunctionOverload;

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    if !is_function(name) {
        return Ok(None);
    }
    let argument_names = argument_names(args);
    let argument_types = effective_argument_types(args, argument_types);
    resolve_overload(name, binding, &argument_names, &argument_types, resolver)
        .map(|overload| Some(overload.return_type))
}

#[doc(hidden)]
pub fn resolve_gamma_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedGammaOverload, SQLError> {
    if !is_function(name) {
        return Err(undefined_function(name, argument_names, argument_types));
    }
    resolve_overload(name, binding, argument_names, argument_types, resolver)
}

pub(super) fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "gamma" | "lgamma"
    )
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() || !is_function(&name) {
        return name;
    }
    let argument_types = args
        .iter()
        .map(|argument| scalar_type_inner(named_argument_value(argument), schema, params, resolver))
        .collect::<Result<Vec<_>, _>>();
    let source_type = argument_types
        .as_ref()
        .ok()
        .and_then(|types| types.first())
        .and_then(Clone::clone);
    let names = argument_names(args);
    let selected = argument_types.and_then(|types| {
        let types = effective_argument_types(args, &types);
        resolve_overload(&name, None, &names, &types, resolver)
    });
    if let Ok(selected) = selected {
        if selected.binding.builtin
            && !source_type
                .as_ref()
                .is_some_and(|ty| matches!(base_type(ty), ColumnType::DoublePrecision))
        {
            if let Some(argument) = args.first_mut() {
                *argument = ScalarExpr::Cast {
                    expr: Box::new(std::mem::replace(
                        argument,
                        ScalarExpr::Literal(Value::Null),
                    )),
                    ty: ColumnType::DoublePrecision.sql_name(),
                };
            }
        }
        *binding = Some(selected.binding);
    }
    name
}

fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedGammaOverload, SQLError> {
    let builtin = builtin(name);
    if let Some(resolver) = resolver {
        if let Some(selected) = resolver.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            std::slice::from_ref(&builtin),
        )? {
            return Ok(selected);
        }
    }
    resolve_local_builtin(name, binding, argument_names, argument_types, builtin)
}

fn resolve_local_builtin(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtin: BuiltinFunctionOverload,
) -> Result<ResolvedGammaOverload, SQLError> {
    if let Some(binding) = binding {
        if !binding.builtin
            || !binding.name.eq_ignore_ascii_case(&builtin.name)
            || binding.argument_types != [ColumnType::DoublePrecision.sql_name()]
        {
            return Err(undefined_function(name, argument_names, argument_types));
        }
    }
    let [argument_name] = argument_names else {
        return Err(undefined_function(name, argument_names, argument_types));
    };
    let [argument_type] = argument_types else {
        return Err(undefined_function(name, argument_names, argument_types));
    };
    if argument_name.is_some()
        || argument_type
            .as_ref()
            .is_some_and(|ty| !accepts_implicit_float8(ty))
    {
        return Err(undefined_function(name, argument_names, argument_types));
    }
    Ok(ResolvedFunctionOverload {
        binding: FunctionBinding {
            name: builtin.name,
            argument_types: vec![ColumnType::DoublePrecision.sql_name()],
            builtin: true,
        },
        return_type: builtin.return_type,
        exact_matches: usize::from(
            argument_type
                .as_ref()
                .is_some_and(|ty| matches!(base_type(ty), ColumnType::DoublePrecision)),
        ),
        known_arguments: usize::from(argument_type.is_some()),
        preferred_matches: usize::from(argument_type.is_none()),
        precedes_pg_catalog: false,
    })
}

fn accepts_implicit_float8(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Numeric { .. }
            | ColumnType::Real
            | ColumnType::DoublePrecision
    )
}

fn builtin(name: &str) -> BuiltinFunctionOverload {
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    BuiltinFunctionOverload {
        name: format!("pg_catalog.{local}"),
        argument_names: vec![None],
        argument_types: vec![ColumnType::DoublePrecision],
        return_type: ColumnType::DoublePrecision,
    }
}

fn effective_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            if matches!(
                named_argument_value(argument),
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                argument_type.clone()
            }
        })
        .collect()
}

fn argument_names(args: &[ScalarExpr]) -> Vec<Option<String>> {
    args.iter()
        .map(|argument| named_argument_name(argument).map(str::to_string))
        .collect()
}

fn named_argument_name(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return None;
    }
    match args.first() {
        Some(ScalarExpr::Literal(Value::Str(name))) => Some(name),
        _ => None,
    }
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let signature = argument_names
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
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}
