//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

use super::common::{
    base_type, common_numeric_type, common_type, merge_optional_types, numeric_type,
};
use super::{
    array_transform, containment, fixed_builtin, integer_base, random_range, scalar_type_inner,
    FunctionTypeResolver,
};

pub fn builtin_function_type(
    name: &str,
    args: &[ScalarExpr],
    order_by: &[crate::ScalarOrder],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    builtin_function_type_inner(name, None, args, order_by, schema, params, None)
}

/// Return the declared argument targets selected by PostgreSQL-compatible built-in resolution. Known argument types are retained for polymorphic calls, while fixed signatures and overloaded operators supply the context needed to resolve `unknown` arguments.
#[must_use]
pub fn builtin_function_argument_targets(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    let lower = name.to_ascii_lowercase();
    let name = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if let Some(targets) = fixed_builtin::selected_argument_targets(name, argument_types) {
        return targets;
    }
    let mut targets = argument_types.to_vec();
    match name {
        "upper" | "lower" | "initcap" | "trim" | "btrim" | "ltrim" | "rtrim" => {
            targets.fill(Some(ColumnType::Text));
        }
        "array_sort" if matches!(targets.len(), 2 | 3) => {
            targets.iter_mut().skip(1).for_each(|target| {
                *target = Some(ColumnType::Boolean);
            });
        }
        "concat_op" if targets.len() == 2 => {
            for position in 0..2 {
                if targets[position].is_none() {
                    targets[position] = Some(concat_argument_type(targets[1 - position].as_ref()));
                }
            }
        }
        _ => {}
    }
    targets
}

pub(super) fn builtin_function_type_inner(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    order_by: &[crate::ScalarOrder],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let original_name = name;
    let lower = name.to_ascii_lowercase();
    let name = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if name == uqa_sql::expr::NAMED_ARG_FUNCTION {
        return args.get(1).map_or(Ok(None), |expression| {
            scalar_type_inner(expression, schema, params, resolver)
        });
    }
    if binding.is_none()
        && resolver.is_some_and(|resolver| resolver.has_untyped_function(original_name))
    {
        return Ok(None);
    }
    if name.contains('.') && resolver.is_none() {
        return Ok(None);
    }
    let argument_types = args
        .iter()
        .map(|argument| scalar_type_inner(named_argument_value(argument), schema, params, resolver))
        .collect::<Result<Vec<_>, _>>()?;
    if name.contains('.') {
        return resolve_extension_function_type(
            resolver,
            original_name,
            binding,
            args,
            &argument_types,
        );
    }
    let ordered_argument_types = order_by
        .iter()
        .map(|order| scalar_type_inner(&order.expr, schema, params, resolver))
        .collect::<Result<Vec<_>, _>>()?;
    let argument = |position: usize| argument_types.get(position).cloned().flatten();
    let ordered_argument = || ordered_argument_types.first().cloned().flatten();
    let first = || argument(0);
    if fixed_builtin::is_function(name) {
        return fixed_builtin::resolve_type(
            original_name,
            binding,
            args,
            &argument_types,
            resolver,
        );
    }
    match name {
        "pg_typeof" => Ok(Some(ColumnType::Regtype)),
        "typeof"
        | "upper"
        | "lower"
        | "initcap"
        | "trim"
        | "btrim"
        | "ltrim"
        | "rtrim"
        | "concat"
        | "concat_ws"
        | "replace"
        | "substring"
        | "substr"
        | "left"
        | "right"
        | "chr"
        | "regexp_replace"
        | "lpad"
        | "rpad"
        | "repeat"
        | "translate"
        | "overlay"
        | "format"
        | "encode"
        | "split_part"
        | "quote_ident"
        | "quote_literal"
        | "quote_nullable"
        | "regexp_substr"
        | "array_to_string"
        | "array_dims"
        | "json_typeof"
        | "jsonb_typeof"
        | "jsonb_pretty"
        | "to_char"
        | "timeofday"
        | "current_setting"
        | "merge_action"
        | "string_to_table"
        | "regexp_split_to_table"
        | "json_object_keys"
        | "jsonb_object_keys"
        | "json_array_elements_text"
        | "jsonb_array_elements_text"
        | "json_extract_path_text"
        | "jsonb_extract_path_text" => Ok(Some(ColumnType::Text)),
        name if integer_base::is_bound_function(name) => Ok(Some(ColumnType::Text)),
        name if random_range::bound_function_type(name).is_some() => {
            Ok(random_range::bound_function_type(name))
        }
        name if array_transform::is_bound_function(name) => Ok(first()),
        "array_sort" | "array_reverse" => {
            array_transform::resolve_type(original_name, binding, args, &argument_types, resolver)
        }
        "count" | "row_number" | "rank" | "dense_rank" | "nextval" | "currval" | "setval" => {
            Ok(Some(ColumnType::BigInteger))
        }
        "sum" => Ok(first().and_then(|ty| aggregate_sum_type(&ty))),
        "avg" => Ok(first().and_then(|ty| aggregate_average_type(&ty))),
        "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
            Ok(first().and_then(|ty| aggregate_average_type(&ty)))
        }
        "min" | "max" | "lag" | "lead" | "first_value" | "last_value" | "nth_value" | "nullif"
        | "array_cat" | "array_remove" | "array_replace" | "trim_array" | "array_sample"
        | "__slice" | "__array_slices" | "array_append" | "generate_series" => Ok(first()),
        "mode" | "percentile_disc" => Ok(ordered_argument()),
        "percentile_cont" => Ok(ordered_argument().map(|ty| match base_type(&ty) {
            ColumnType::Interval => ColumnType::Interval,
            _ => ColumnType::DoublePrecision,
        })),
        "array_agg" => Ok(first().map(|ty| ColumnType::Array(Box::new(ty)))),
        "string_agg" => Ok(first().map(|ty| {
            if matches!(ty, ColumnType::Bytea) {
                ColumnType::Bytea
            } else {
                ColumnType::Text
            }
        })),
        "json_agg"
        | "json_object_agg"
        | "json_array_elements"
        | "json_extract_path"
        | "to_json"
        | "row_to_json"
        | "json_build_object"
        | "json_build_array" => Ok(Some(ColumnType::Json)),
        "jsonb_agg"
        | "jsonb_object_agg"
        | "jsonb_array_elements"
        | "jsonb_extract_path"
        | "json_delete_path"
        | "jsonb_set"
        | "jsonb_insert"
        | "to_jsonb"
        | "jsonb_build_object"
        | "jsonb_build_array" => Ok(Some(ColumnType::JsonB)),
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            Ok(Some(ColumnType::Record))
        }
        "contains_op" | "contained_by_op" => {
            containment::resolve_operator_type(name, args, &argument_types)
        }
        "bool_and"
        | "bool_or"
        | "every"
        | "starts_with"
        | "like"
        | "ilike"
        | "similar_to"
        | "regexp_like"
        | "isfinite"
        | "json_contains"
        | "json_contained_by"
        | "json_has_key"
        | "json_has_any_key"
        | "json_has_all_keys"
        | "jsonb_path_exists"
        | "jsonpath_exists"
        | "jsonb_path_match"
        | "jsonpath_match"
        | "array_overlap"
        | "__any_op"
        | "__all_op"
        | "__is_distinct"
        | "__between_symmetric"
        | "st_within"
        | "st_dwithin"
        | "overlaps" => Ok(Some(ColumnType::Boolean)),
        "coalesce" | "greatest" | "least" => common_argument_type(args, &argument_types),
        "concat_op" => concat_type(argument(0), argument(1)),
        "ntile" | "position" | "strpos" | "ascii" | "width_bucket" | "regexp_count"
        | "regexp_instr" | "num_nulls" | "num_nonnulls" | "array_length" | "array_upper"
        | "array_lower" | "array_ndims" | "cardinality" | "array_position"
        | "json_array_length" | "jsonb_array_length" => Ok(Some(ColumnType::Integer)),
        "abs" => Ok(first().map(|ty| base_type(&ty).clone())),
        "round" | "trunc" | "ceil" | "ceiling" | "floor" | "sign" => {
            Ok(first().map(|ty| numeric_unary_result_type(&ty)))
        }
        "mod" | "gcd" | "lcm" => numeric_binary_function_type(argument(0), argument(1)),
        "div" | "factorial" | "extract" | "to_number" => Ok(Some(numeric_type())),
        "power" | "pow" => numeric_power_type(args, &argument_types),
        "sqrt" | "ln" | "log" | "log10" => numeric_transcendental_type(args, &argument_types),
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh"
        | "exp" | "log2" | "cbrt" | "degrees" | "radians" | "pi" | "st_distance" | "date_part" => {
            Ok(Some(ColumnType::DoublePrecision))
        }
        "regexp_match" | "regexp_matches" | "string_to_array" => {
            Ok(Some(ColumnType::Array(Box::new(ColumnType::Text))))
        }
        "array_positions" => Ok(Some(ColumnType::Array(Box::new(ColumnType::Integer)))),
        "decode" => Ok(Some(ColumnType::Bytea)),
        "array_prepend" => Ok(argument(1)),
        "array_fill" => Ok(first().map(|ty| ColumnType::Array(Box::new(ty)))),
        "__subscript" | "__array_subscripts" | "unnest" => Ok(first().and_then(array_element_type)),
        "now" | "current_timestamp" | "clock_timestamp" | "statement_timestamp" => {
            Ok(Some(ColumnType::TimestampTz))
        }
        "current_date" | "make_date" | "to_date" => Ok(Some(ColumnType::Date)),
        "to_timestamp" => Ok(Some(ColumnType::TimestampTz)),
        "age" | "make_interval" | "justify_hours" => Ok(Some(ColumnType::Interval)),
        "date_trunc" => Ok(argument(1).map(|ty| match base_type(&ty) {
            ColumnType::Interval => ColumnType::Interval,
            ColumnType::Timestamp => ColumnType::Timestamp,
            _ => ColumnType::TimestampTz,
        })),
        "make_timestamp" => Ok(Some(ColumnType::Timestamp)),
        "current_database" | "current_catalog" | "current_schema" | "current_user"
        | "session_user" => Ok(Some(ColumnType::Name)),
        "current_schemas" => Ok(Some(ColumnType::Array(Box::new(ColumnType::Name)))),
        _ => {
            resolve_extension_function_type(resolver, original_name, binding, args, &argument_types)
        }
    }
}

fn resolve_extension_function_type(
    resolver: Option<&dyn FunctionTypeResolver>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    resolved_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for (argument, resolved_type) in args.iter().zip(resolved_types) {
        let (name, value) = named_argument(argument);
        argument_names.push(name);
        argument_types.push(
            if matches!(value, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                None
            } else {
                resolved_type.clone()
            },
        );
    }
    resolver.resolve_function_type(name, binding, &argument_names, &argument_types)
}

pub(super) fn named_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return (None, expression);
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return (None, expression);
    }
    let argument_name = args.first().and_then(|name| match name {
        ScalarExpr::Literal(Value::Str(name)) => Some(name.clone()),
        _ => None,
    });
    (argument_name, named_argument_value(expression))
}

pub(super) fn named_argument_value(expression: &ScalarExpr) -> &ScalarExpr {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return expression;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return expression;
    }
    args.get(1).unwrap_or(expression)
}

fn aggregate_sum_type(ty: &ColumnType) -> Option<ColumnType> {
    Some(match base_type(ty) {
        ColumnType::SmallInteger | ColumnType::Integer => ColumnType::BigInteger,
        ColumnType::BigInteger | ColumnType::Numeric { .. } => numeric_type(),
        ColumnType::Real => ColumnType::Real,
        ColumnType::DoublePrecision => ColumnType::DoublePrecision,
        _ => return None,
    })
}

fn aggregate_average_type(ty: &ColumnType) -> Option<ColumnType> {
    Some(match base_type(ty) {
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Numeric { .. } => numeric_type(),
        ColumnType::Real | ColumnType::DoublePrecision => ColumnType::DoublePrecision,
        _ => return None,
    })
}

fn common_argument_type(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let mut result = None;
    for (argument, argument_type) in args.iter().zip(argument_types) {
        result = merge_optional_types(
            result,
            if matches!(
                named_argument_value(argument),
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                argument_type.clone()
            },
        )?;
    }
    Ok(result.or(Some(ColumnType::Text)))
}

fn concat_type(
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (Some(ColumnType::Array(left)), Some(ColumnType::Array(right))) => {
            common_type(&left, &right).map(|element| Some(ColumnType::Array(Box::new(element))))
        }
        (Some(array @ ColumnType::Array(_)), _) | (_, Some(array @ ColumnType::Array(_))) => {
            Ok(Some(array))
        }
        (Some(ColumnType::JsonB), Some(ColumnType::JsonB)) => Ok(Some(ColumnType::JsonB)),
        _ => Ok(Some(ColumnType::Text)),
    }
}

fn concat_argument_type(other: Option<&ColumnType>) -> ColumnType {
    match other {
        Some(array @ ColumnType::Array(_)) => array.clone(),
        Some(ColumnType::JsonB) => ColumnType::JsonB,
        _ => ColumnType::Text,
    }
}

fn numeric_unary_result_type(ty: &ColumnType) -> ColumnType {
    if matches!(base_type(ty), ColumnType::Numeric { .. }) {
        numeric_type()
    } else {
        ColumnType::DoublePrecision
    }
}

fn numeric_binary_function_type(
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (Some(left), Some(right)) => common_numeric_type(base_type(&left), base_type(&right))
            .map(Some)
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "types {} and {} are not numeric",
                    left.sql_name(),
                    right.sql_name()
                ))
            }),
        (Some(ty), None) | (None, Some(ty)) => Ok(Some(base_type(&ty).clone())),
        (None, None) => Ok(None),
    }
}

fn numeric_transcendental_type(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let mut saw_argument = false;
    for argument_type in argument_types {
        let Some(ty) = argument_type else {
            continue;
        };
        saw_argument = true;
        if !matches!(base_type(ty), ColumnType::Numeric { .. }) {
            return Ok(Some(ColumnType::DoublePrecision));
        }
    }
    Ok(if saw_argument {
        Some(numeric_type())
    } else if args.is_empty() {
        None
    } else {
        Some(ColumnType::DoublePrecision)
    })
}

fn numeric_power_type(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let mut saw_numeric = false;
    let mut saw_floating = false;
    for (argument, argument_type) in args.iter().zip(argument_types) {
        let argument = named_argument_value(argument);
        if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
            continue;
        }
        let Some(ty) = argument_type else {
            continue;
        };
        match base_type(ty) {
            ColumnType::Numeric { .. } => saw_numeric = true,
            ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger => {}
            ColumnType::Real | ColumnType::DoublePrecision => saw_floating = true,
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: "function power with these argument types does not exist".into(),
                })
            }
        }
    }
    Ok(if saw_floating {
        Some(ColumnType::DoublePrecision)
    } else if saw_numeric {
        Some(numeric_type())
    } else if !args.is_empty() {
        Some(ColumnType::DoublePrecision)
    } else {
        None
    })
}

fn array_element_type(ty: ColumnType) -> Option<ColumnType> {
    match ty {
        ColumnType::Array(element) => Some(*element),
        ColumnType::Int2Vector => Some(ColumnType::SmallInteger),
        ColumnType::OidVector => Some(ColumnType::Oid),
        _ => None,
    }
}
