//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `json_strip_nulls` and `jsonb_strip_nulls`.

use super::common::base_type;
use super::functions::named_argument_value;
use super::{
    scalar_type_inner, BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload,
};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

pub type ResolvedJsonStripOverload = ResolvedFunctionOverload;

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
pub fn resolve_json_strip_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedJsonStripOverload, SQLError> {
    if !is_function(name) {
        return Err(undefined_function(name, argument_names, argument_types));
    }
    resolve_overload(name, binding, argument_names, argument_types, resolver)
}

pub(super) fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "json_strip_nulls" | "jsonb_strip_nulls"
    )
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if !is_function(&name) {
        return name;
    }
    let argument_types = args
        .iter()
        .map(|argument| scalar_type_inner(named_argument_value(argument), schema, params, resolver))
        .collect::<Result<Vec<_>, _>>();
    let names = argument_names(args);
    let selected = argument_types.and_then(|types| {
        let types = effective_argument_types(args, &types);
        resolve_overload(&name, binding.as_ref(), &names, &types, resolver)
    });
    let Ok(selected) = selected else {
        return name;
    };
    if !selected.binding.builtin {
        *binding = Some(selected.binding);
        return name;
    }
    let position_names = names.iter().map(Option::as_deref).collect::<Vec<_>>();
    let Ok(Some(positions)) =
        uqa_sql::expr::json_strip_nulls_argument_positions(&name, &position_names)
    else {
        return name;
    };
    let mut reordered = vec![None; selected.binding.argument_types.len()];
    for (argument, position) in std::mem::take(args).into_iter().zip(positions) {
        reordered[position] = Some(named_argument_value_owned(argument));
    }
    reordered[1].get_or_insert(ScalarExpr::Literal(Value::Bool(false)));
    *args = reordered
        .into_iter()
        .map(|argument| argument.expect("JSON null stripping fills every declared argument slot"))
        .collect();
    for (argument, declared) in args.iter_mut().zip(&selected.binding.argument_types) {
        let declared_type = ColumnType::from_sql_name(declared)
            .expect("built-in JSON null stripping bindings use catalogued SQL types");
        let actual = scalar_type_inner(argument, schema, params, resolver)
            .ok()
            .flatten();
        let requires_cast = matches!(
            argument,
            ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
        ) || actual
            .as_ref()
            .is_none_or(|actual| base_type(actual) != &declared_type);
        if requires_cast {
            *argument = ScalarExpr::Cast {
                expr: Box::new(std::mem::replace(
                    argument,
                    ScalarExpr::Literal(Value::Null),
                )),
                ty: declared.clone(),
            };
        }
    }
    *binding = Some(selected.binding);
    name
}

fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedJsonStripOverload, SQLError> {
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
) -> Result<ResolvedJsonStripOverload, SQLError> {
    if let Some(binding) = binding {
        let expected_types = builtin
            .argument_types
            .iter()
            .map(ColumnType::sql_name)
            .collect::<Vec<_>>();
        if !binding.builtin
            || !binding.name.eq_ignore_ascii_case(&builtin.name)
            || binding.argument_types != expected_types
        {
            return Err(undefined_function(name, argument_names, argument_types));
        }
    }
    let names = argument_names
        .iter()
        .map(Option::as_deref)
        .collect::<Vec<_>>();
    let Some(positions) = uqa_sql::expr::json_strip_nulls_argument_positions(name, &names)? else {
        return Err(undefined_function(name, argument_names, argument_types));
    };
    let mut exact_matches = 0usize;
    for (actual, position) in argument_types.iter().zip(positions) {
        let Some(actual) = actual else {
            continue;
        };
        let declared = &builtin.argument_types[position];
        if base_type(actual) != declared {
            return Err(undefined_function(name, argument_names, argument_types));
        }
        exact_matches += usize::from(actual == declared);
    }
    let known_arguments = argument_types.iter().flatten().count();
    Ok(ResolvedFunctionOverload {
        binding: FunctionBinding {
            name: builtin.name,
            argument_types: builtin
                .argument_types
                .iter()
                .map(ColumnType::sql_name)
                .collect(),
            builtin: true,
        },
        return_type: builtin.return_type,
        exact_matches,
        known_arguments,
        preferred_matches: 0,
        precedes_pg_catalog: false,
    })
}

fn builtin(name: &str) -> BuiltinFunctionOverload {
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    let target = if local == "jsonb_strip_nulls" {
        ColumnType::JsonB
    } else {
        ColumnType::Json
    };
    BuiltinFunctionOverload {
        name: format!("pg_catalog.{local}"),
        argument_names: vec![Some("target".into()), Some("strip_in_arrays".into())],
        argument_types: vec![target.clone(), ColumnType::Boolean],
        default_arguments: 1,
        return_type: target,
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
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
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

fn named_argument_value_owned(expression: ScalarExpr) -> ScalarExpr {
    if matches!(
        &expression,
        ScalarExpr::Func { name, args, .. }
            if name == uqa_sql::expr::NAMED_ARG_FUNCTION && args.len() == 2
    ) {
        let ScalarExpr::Func { mut args, .. } = expression else {
            unreachable!();
        };
        return args.pop().expect("named argument value follows its name");
    }
    expression
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
