//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `array_sort` and `array_reverse`.

use super::common::{base_type, common_context_expression_type};
use super::functions::named_argument_value;
use super::{FunctionTypeResolver, ResolvedFunctionOverload};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::expr::ARRAY_SORT_JSON_FUNCTION;
use uqa_sql::{SQLError, SQLParam};

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    select_overload(name, binding, args, argument_types, resolver).map(|selected| {
        Some(match selected {
            SelectedOverload::Builtin(return_type) => return_type,
            SelectedOverload::User(resolved) => resolved.return_type,
        })
    })
}

enum SelectedOverload {
    Builtin(ColumnType),
    User(ResolvedFunctionOverload),
}

fn select_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<SelectedOverload, SQLError> {
    let builtin = resolve_builtin_type(name, args, argument_types);
    let user = match resolve_user_overload(name, binding, args, argument_types, resolver) {
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => None,
        other => other?,
    };
    if binding.is_some() {
        return user.map(SelectedOverload::User).ok_or_else(|| {
            builtin.err().unwrap_or_else(|| {
                undefined_function(name, args, &user_argument_types(args, argument_types))
            })
        });
    }
    match (builtin, user) {
        (Ok(return_type), None) => Ok(SelectedOverload::Builtin(return_type)),
        (Ok(_), Some(user)) if user.is_exact_for_known_arguments() => {
            Ok(SelectedOverload::User(user))
        }
        (Ok(_), Some(_)) => Err(ambiguous_function(name, args, argument_types)),
        (Err(_), Some(user)) => Ok(SelectedOverload::User(user)),
        (Err(error), None) => Err(error),
    }
}

fn resolve_builtin_type(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<ColumnType, SQLError> {
    let argument_names = args.iter().map(named_argument_name).collect::<Vec<_>>();
    let Some(positions) = uqa_sql::expr::array_transform_argument_positions(name, &argument_names)?
    else {
        return Err(undefined_function(name, args, argument_types));
    };
    let effective_types = args
        .iter()
        .zip(argument_types)
        .zip(&positions)
        .map(|((argument, argument_type), position)| {
            let argument = named_argument_value(argument);
            if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null))
                || *position > 0 && matches!(argument, ScalarExpr::Param(_))
            {
                None
            } else {
                argument_type.clone()
            }
        })
        .collect::<Vec<_>>();
    let mut declared_types = vec![None; args.len()];
    for (argument_type, position) in effective_types.iter().cloned().zip(positions) {
        declared_types[position] = argument_type;
    }
    if declared_types.iter().skip(1).any(|argument_type| {
        argument_type
            .as_ref()
            .is_some_and(|argument_type| !matches!(base_type(argument_type), ColumnType::Boolean))
    }) {
        return Err(undefined_function(name, args, &effective_types));
    }
    match declared_types.first().cloned().flatten() {
        Some(argument_type) if is_array_type(&argument_type) => {
            Ok(base_type(&argument_type).clone())
        }
        None => Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "could not determine polymorphic type because input has type unknown".into(),
        }),
        Some(_) => Err(undefined_function(name, args, &effective_types)),
    }
}

fn resolve_user_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
    if binding.is_none() && name.to_ascii_lowercase().starts_with("pg_catalog.") {
        return Ok(None);
    }
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let argument_names = args
        .iter()
        .map(|argument| named_argument_name(argument).map(str::to_string))
        .collect::<Vec<_>>();
    resolver.resolve_function_overload(
        name,
        binding,
        &argument_names,
        &user_argument_types(args, argument_types),
    )
}

fn user_argument_types(
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

pub(super) fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "array_sort" | "array_reverse"
    )
}

pub(super) fn is_bound_function(name: &str) -> bool {
    name == ARRAY_SORT_JSON_FUNCTION
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() || !is_function(&name) {
        return name;
    }
    let argument_types = args
        .iter()
        .map(|argument| {
            super::scalar_type_inner(named_argument_value(argument), schema, params, resolver)
        })
        .collect::<Result<Vec<_>, _>>();
    if let Ok(SelectedOverload::User(resolved)) = argument_types
        .and_then(|argument_types| select_overload(&name, None, args, &argument_types, resolver))
    {
        *binding = Some(resolved.binding);
        return name;
    }
    let argument_names = args.iter().map(named_argument_name).collect::<Vec<_>>();
    let Ok(Some(positions)) =
        uqa_sql::expr::array_transform_argument_positions(&name, &argument_names)
    else {
        return name;
    };
    let mut reordered = vec![None; args.len()];
    for (argument, position) in std::mem::take(args).into_iter().zip(positions) {
        reordered[position] = Some(named_argument_value_owned(argument));
    }
    *args = reordered
        .into_iter()
        .map(|argument| argument.expect("array transform positions fill every argument slot"))
        .collect();
    for argument in args.iter_mut().skip(1) {
        if matches!(argument, ScalarExpr::Param(_))
            || common_context_expression_type(argument, schema, params, resolver)
                .ok()
                .flatten()
                .is_none()
        {
            *argument = ScalarExpr::Cast {
                expr: Box::new(std::mem::replace(
                    argument,
                    ScalarExpr::Literal(Value::Null),
                )),
                ty: "boolean".into(),
            };
        }
    }
    let lower = name.to_ascii_lowercase();
    let function = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if function == "array_sort"
        && args
            .first()
            .and_then(|argument| {
                super::scalar_type_inner(argument, schema, params, resolver)
                    .ok()
                    .flatten()
            })
            .as_ref()
            .is_some_and(is_json_array_type)
    {
        ARRAY_SORT_JSON_FUNCTION.into()
    } else {
        name
    }
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

fn is_array_type(argument_type: &ColumnType) -> bool {
    matches!(
        base_type(argument_type),
        ColumnType::Array(_)
            | ColumnType::AnyArray
            | ColumnType::Int2Vector
            | ColumnType::OidVector
    )
}

fn is_json_array_type(argument_type: &ColumnType) -> bool {
    matches!(
        base_type(argument_type),
        ColumnType::Array(element) if matches!(base_type(element), ColumnType::Json)
    )
}

fn undefined_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    function_resolution_error("42883", "does not exist", name, args, argument_types)
}

fn ambiguous_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let argument_types = user_argument_types(args, argument_types);
    function_resolution_error("42725", "is not unique", name, args, &argument_types)
}

fn function_resolution_error(
    sqlstate: &str,
    description: &str,
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let signature = args
        .iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            named_argument_name(argument).map_or(argument_type.clone(), |argument_name| {
                format!("{argument_name} => {argument_type}")
            })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("function {name}({signature}) {description}"),
    }
}
