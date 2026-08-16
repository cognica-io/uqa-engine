//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL type propagation and PostgreSQL-compatible common-type rules.

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, ColumnType, FunctionBinding};
use uqa_sql::expr::{TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION};
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

mod containment;
mod to_hex;

pub trait FunctionTypeResolver: Send + Sync {
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
    ) -> Result<Option<ColumnType>, SQLError>;
}

pub fn scalar_type(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, None)
}

pub fn scalar_type_with_resolver(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, Some(resolver))
}

fn scalar_type_inner(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    match expression {
        ScalarExpr::Column(column) => Ok(schema.type_of(column).cloned()),
        ScalarExpr::Position(position) => Ok(schema.column_type(*position).cloned()),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            Ok(schema.qualified_type(qualifier, column).cloned())
        }
        ScalarExpr::Literal(value) => Ok(value_type(value)),
        ScalarExpr::Param(index) => Ok(index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .and_then(parameter_type)),
        ScalarExpr::Cast { expr, ty } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            ColumnType::from_sql_name(ty).map(Some)
        }
        ScalarExpr::Array(items) => {
            let mut element = None;
            for item in items {
                element = merge_optional_types(
                    element,
                    common_context_expression_type(item, schema, params, resolver)?,
                )?;
            }
            Ok(element.map(|element| ColumnType::Array(Box::new(element))))
        }
        ScalarExpr::Row(items) => {
            for item in items {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Record))
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = scalar_type_inner(lhs, schema, params, resolver)?;
            let right = scalar_type_inner(rhs, schema, params, resolver)?;
            binary_result_type(*op, left.as_ref(), right.as_ref())
        }
        ScalarExpr::UnaryMinus(inner) => scalar_type_inner(inner, schema, params, resolver)?
            .map_or(Ok(None), |ty| unary_minus_result_type(&ty).map(Some)),
        ScalarExpr::Not(inner) | ScalarExpr::IsNull { expr: inner, .. } => {
            scalar_type_inner(inner, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::Between { expr, low, high } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            scalar_type_inner(low, schema, params, resolver)?;
            scalar_type_inner(high, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InList { expr, list, .. } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            for item in list {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InSubquery { expr, .. } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::Exists { .. } => Ok(Some(ColumnType::Boolean)),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                scalar_type_inner(base, schema, params, resolver)?;
            }
            let mut result = None;
            for (condition, value) in when {
                scalar_type_inner(condition, schema, params, resolver)?;
                result = merge_optional_types(
                    result,
                    common_context_expression_type(value, schema, params, resolver)?,
                )?;
            }
            if let Some(value) = else_branch {
                result = merge_optional_types(
                    result,
                    common_context_expression_type(value, schema, params, resolver)?,
                )?;
            }
            Ok(result)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            if let Some(filter) = filter {
                scalar_type_inner(filter, schema, params, resolver)?;
            }
            builtin_function_type_inner(
                name,
                binding.as_ref(),
                args,
                order_by,
                schema,
                params,
                resolver,
            )
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            for expression in &spec.partition_by {
                scalar_type_inner(expression, schema, params, resolver)?;
            }
            for order in &spec.order_by {
                scalar_type_inner(&order.expr, schema, params, resolver)?;
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    match bound {
                        crate::ScalarFrameBound::Preceding(expression)
                        | crate::ScalarFrameBound::Following(expression) => {
                            scalar_type_inner(expression, schema, params, resolver)?;
                        }
                        crate::ScalarFrameBound::UnboundedPreceding
                        | crate::ScalarFrameBound::UnboundedFollowing
                        | crate::ScalarFrameBound::CurrentRow => {}
                    }
                }
            }
            builtin_function_type_inner(name, None, args, &[], schema, params, resolver)
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default => Ok(None),
    }
}

pub fn builtin_function_type(
    name: &str,
    args: &[ScalarExpr],
    order_by: &[crate::ScalarOrder],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    builtin_function_type_inner(name, None, args, order_by, schema, params, None)
}

fn builtin_function_type_inner(
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
    if name.contains('.') {
        return resolve_extension_function_type(
            resolver,
            original_name,
            binding,
            args,
            schema,
            params,
        );
    }
    if name == uqa_sql::expr::NAMED_ARG_FUNCTION {
        return args.get(1).map_or(Ok(None), |expression| {
            scalar_type_inner(expression, schema, params, resolver)
        });
    }
    let argument = |position: usize| -> Result<Option<ColumnType>, SQLError> {
        args.get(position).map_or(Ok(None), |expression| {
            scalar_type_inner(named_argument_value(expression), schema, params, resolver)
        })
    };
    let ordered_argument = || -> Result<Option<ColumnType>, SQLError> {
        order_by.first().map_or(Ok(None), |order| {
            scalar_type_inner(&order.expr, schema, params, resolver)
        })
    };
    let first = || argument(0);
    for argument in args {
        scalar_type_inner(named_argument_value(argument), schema, params, resolver)?;
    }
    for order in order_by {
        scalar_type_inner(&order.expr, schema, params, resolver)?;
    }
    match name {
        "pg_typeof" => Ok(Some(ColumnType::Regtype)),
        "typeof"
        | "upper"
        | "lower"
        | "casefold"
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
        | "md5"
        | "encode"
        | "split_part"
        | TO_HEX_INT4_FUNCTION
        | TO_HEX_INT8_FUNCTION
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
        "to_hex" => to_hex::resolve_type(original_name, args, schema, params, resolver),
        "count" | "row_number" | "rank" | "dense_rank" | "crc32" | "crc32c" | "nextval"
        | "currval" | "setval" => Ok(Some(ColumnType::BigInteger)),
        "sum" => Ok(first()?.and_then(|ty| aggregate_sum_type(&ty))),
        "avg" => Ok(first()?.and_then(|ty| aggregate_average_type(&ty))),
        "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
            Ok(first()?.and_then(|ty| aggregate_average_type(&ty)))
        }
        "min" | "max" | "lag" | "lead" | "first_value" | "last_value" | "nth_value" | "nullif"
        | "array_cat" | "array_remove" | "array_replace" | "trim_array" | "array_sample"
        | "array_reverse" | "array_sort" | "__slice" | "__array_slices" | "array_append"
        | "generate_series" => first(),
        "mode" | "percentile_disc" => ordered_argument(),
        "percentile_cont" => Ok(ordered_argument()?.map(|ty| match base_type(&ty) {
            ColumnType::Interval => ColumnType::Interval,
            _ => ColumnType::DoublePrecision,
        })),
        "array_agg" => Ok(first()?.map(|ty| ColumnType::Array(Box::new(ty)))),
        "string_agg" => Ok(first()?.map(|ty| {
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
        | "json_strip_nulls"
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
        | "jsonb_strip_nulls"
        | "to_jsonb"
        | "jsonb_build_object"
        | "jsonb_build_array" => Ok(Some(ColumnType::JsonB)),
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            Ok(Some(ColumnType::Record))
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
        | "contains_op"
        | "contained_by_op"
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
        | "overlaps" => {
            if matches!(name, "contains_op" | "contained_by_op") {
                return containment::resolve_operator_type(name, args, schema, params, resolver);
            }
            Ok(Some(ColumnType::Boolean))
        }
        "coalesce" | "greatest" | "least" => common_argument_type(args, schema, params, resolver),
        "reverse" => Ok(first()?.map(|ty| {
            if matches!(base_type(&ty), ColumnType::Bytea) {
                ColumnType::Bytea
            } else {
                ColumnType::Text
            }
        })),
        "concat_op" => concat_type(argument(0)?, argument(1)?),
        "ntile" | "length" | "char_length" | "character_length" | "octet_length" | "position"
        | "strpos" | "ascii" | "width_bucket" | "bit_length" | "regexp_count" | "regexp_instr"
        | "num_nulls" | "num_nonnulls" | "array_length" | "array_upper" | "array_lower"
        | "array_ndims" | "cardinality" | "array_position" | "json_array_length"
        | "jsonb_array_length" => Ok(Some(ColumnType::Integer)),
        "abs" => Ok(first()?.map(|ty| base_type(&ty).clone())),
        "round" | "trunc" | "ceil" | "ceiling" | "floor" | "sign" => {
            Ok(first()?.map(|ty| numeric_unary_result_type(&ty)))
        }
        "mod" | "gcd" | "lcm" => numeric_binary_function_type(argument(0)?, argument(1)?),
        "div" | "factorial" | "extract" | "to_number" => Ok(Some(numeric_type())),
        "power" | "pow" => numeric_power_type(args, schema, params, resolver),
        "sqrt" | "ln" | "log" | "log10" => {
            numeric_transcendental_type(args, schema, params, resolver)
        }
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh"
        | "exp" | "log2" | "cbrt" | "gamma" | "lgamma" | "degrees" | "radians" | "pi"
        | "random" | "st_distance" | "date_part" => Ok(Some(ColumnType::DoublePrecision)),
        "regexp_match" | "regexp_matches" | "string_to_array" => {
            Ok(Some(ColumnType::Array(Box::new(ColumnType::Text))))
        }
        "array_positions" => Ok(Some(ColumnType::Array(Box::new(ColumnType::Integer)))),
        "decode" => Ok(Some(ColumnType::Bytea)),
        "array_prepend" => argument(1),
        "array_fill" => Ok(first()?.map(|ty| ColumnType::Array(Box::new(ty)))),
        "__subscript" | "__array_subscripts" | "unnest" => {
            Ok(first()?.and_then(array_element_type))
        }
        "now" | "current_timestamp" | "clock_timestamp" | "statement_timestamp" => {
            Ok(Some(ColumnType::TimestampTz))
        }
        "current_date" | "make_date" | "to_date" => Ok(Some(ColumnType::Date)),
        "to_timestamp" => Ok(Some(ColumnType::TimestampTz)),
        "age" | "make_interval" | "justify_hours" => Ok(Some(ColumnType::Interval)),
        "date_trunc" => Ok(argument(1)?.map(|ty| match base_type(&ty) {
            ColumnType::Interval => ColumnType::Interval,
            ColumnType::Timestamp => ColumnType::Timestamp,
            _ => ColumnType::TimestampTz,
        })),
        "make_timestamp" => Ok(Some(ColumnType::Timestamp)),
        "gen_random_uuid" | "uuidv4" | "uuidv7" => Ok(Some(ColumnType::Uuid)),
        "current_database" | "current_catalog" | "current_schema" | "current_user"
        | "session_user" => Ok(Some(ColumnType::Name)),
        "current_schemas" => Ok(Some(ColumnType::Array(Box::new(ColumnType::Name)))),
        _ => {
            resolve_extension_function_type(resolver, original_name, binding, args, schema, params)
        }
    }
}

fn resolve_extension_function_type(
    resolver: Option<&dyn FunctionTypeResolver>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for argument in args {
        let (name, value) = named_argument(argument);
        argument_names.push(name);
        argument_types.push(
            if matches!(value, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                None
            } else {
                scalar_type_inner(value, schema, params, Some(resolver))?
            },
        );
    }
    resolver.resolve_function_type(name, binding, &argument_names, &argument_types)
}

fn named_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
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
    (argument_name, args.get(1).unwrap_or(expression))
}

fn named_argument_value(expression: &ScalarExpr) -> &ScalarExpr {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return expression;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return expression;
    }
    args.get(1).unwrap_or(expression)
}

fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
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
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let mut result = None;
    for argument in args {
        result = merge_optional_types(
            result,
            common_context_expression_type(
                named_argument_value(argument),
                schema,
                params,
                resolver,
            )?,
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

fn numeric_unary_result_type(ty: &ColumnType) -> ColumnType {
    if matches!(base_type(ty), ColumnType::Numeric { .. }) {
        numeric_type()
    } else {
        ColumnType::DoublePrecision
    }
}

fn unary_minus_result_type(ty: &ColumnType) -> Result<ColumnType, SQLError> {
    match base_type(ty) {
        ty @ (ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Interval) => Ok(ty.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "operator does not exist: - {}",
            other.sql_name()
        ))),
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
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let mut saw_argument = false;
    for argument in args {
        let Some(ty) = scalar_type_inner(named_argument_value(argument), schema, params, resolver)?
        else {
            continue;
        };
        saw_argument = true;
        if !matches!(base_type(&ty), ColumnType::Numeric { .. }) {
            return Ok(Some(ColumnType::DoublePrecision));
        }
    }
    Ok(saw_argument.then(numeric_type))
}

fn numeric_power_type(
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let mut saw_numeric = false;
    let mut saw_floating = false;
    for argument in args {
        let argument = named_argument_value(argument);
        if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
            continue;
        }
        let Some(ty) = scalar_type_inner(argument, schema, params, resolver)? else {
            continue;
        };
        match base_type(&ty) {
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

fn binary_result_type(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return Ok(Some(ColumnType::Boolean));
    }
    let (Some(left), Some(right)) = (left, right) else {
        return merge_optional_types(left.cloned(), right.cloned());
    };
    let left = base_type(left);
    let right = base_type(right);
    if let Some(ty) = temporal_binary_result_type(op, left, right) {
        return Ok(Some(ty));
    }
    if let Some(ty) = common_numeric_type(left, right) {
        return Ok(Some(ty));
    }
    if matches!(left, ColumnType::JsonB)
        && matches!(op, BinaryOp::Subtract)
        && (right.is_character_string()
            || matches!(right, ColumnType::SmallInteger | ColumnType::Integer)
            || matches!(right, ColumnType::Array(element) if element.is_character_string()))
    {
        return Ok(Some(ColumnType::JsonB));
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} {} {}",
            left.sql_name(),
            binary_operator_name(op),
            right.sql_name()
        ),
    })
}

fn temporal_binary_result_type(
    op: BinaryOp,
    left: &ColumnType,
    right: &ColumnType,
) -> Option<ColumnType> {
    use ColumnType as T;
    match (left, right, op) {
        (T::Date, T::Date, BinaryOp::Subtract) => Some(T::Integer),
        (T::Date, T::SmallInteger | T::Integer, BinaryOp::Add | BinaryOp::Subtract)
        | (T::SmallInteger | T::Integer, T::Date, BinaryOp::Add) => Some(T::Date),
        (T::Date | T::Timestamp, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::Date | T::Timestamp, BinaryOp::Add) => Some(T::Timestamp),
        (T::TimestampTz, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::TimestampTz, BinaryOp::Add) => Some(T::TimestampTz),
        (T::Time, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::Time, BinaryOp::Add) => Some(T::Time),
        (T::TimeTz, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::TimeTz, BinaryOp::Add) => Some(T::TimeTz),
        (T::Interval, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Time, T::Time, BinaryOp::Subtract)
        | (T::TimeTz, T::TimeTz, BinaryOp::Subtract)
        | (
            T::Date | T::Timestamp | T::TimestampTz,
            T::Date | T::Timestamp | T::TimestampTz,
            BinaryOp::Subtract,
        ) => Some(T::Interval),
        (T::Interval, ty, BinaryOp::Multiply | BinaryOp::Divide) if numeric_rank(ty).is_some() => {
            Some(T::Interval)
        }
        (ty, T::Interval, BinaryOp::Multiply) if numeric_rank(ty).is_some() => Some(T::Interval),
        _ => None,
    }
}

fn binary_operator_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Equal => "=",
        BinaryOp::NotEqual => "<>",
        BinaryOp::Less => "<",
        BinaryOp::LessEqual => "<=",
        BinaryOp::Greater => ">",
        BinaryOp::GreaterEqual => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Subtract => "-",
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
    }
}

/// Bind polymorphic type-introspection calls and common-type coercions while
/// the input schema still carries declared SQL types. Runtime values
/// deliberately do not encode integer widths, varchar identity, or float
/// widths, and selector expressions must return the common SQL type rather
/// than the storage type of the branch selected at runtime.
pub fn bind_type_introspection(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> ScalarExpr {
    bind_type_introspection_inner(expression, schema, params, None)
}

/// Bind type-introspection calls with access to catalog-backed function and aggregate overloads.
pub fn bind_type_introspection_with_resolver(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> ScalarExpr {
    bind_type_introspection_inner(expression, schema, params, Some(resolver))
}

fn bind_type_introspection_inner(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> ScalarExpr {
    if !requires_type_introspection_binding(&expression) {
        return expression;
    }
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            mut args,
            distinct,
            mut order_by,
            mut filter,
        } => {
            for argument in &mut args {
                bind_type_introspection_in_place(argument, schema, params, resolver);
            }
            for order in &mut order_by {
                bind_type_introspection_in_place(&mut order.expr, schema, params, resolver);
            }
            if let Some(filter) = filter.as_deref_mut() {
                bind_type_introspection_in_place(filter, schema, params, resolver);
            }
            if containment::is_operator(&name) {
                containment::bind_unknown_arguments(&mut args, schema, params, resolver);
            }
            if is_common_type_function(&name) {
                bind_common_type_expressions(&mut args, schema, params, resolver);
            }
            let name =
                to_hex::bind_overload(name, binding.as_ref(), &args, schema, params, resolver);
            if is_pg_typeof(&name) && args.len() == 1 {
                let name = scalar_type_inner(&args[0], schema, params, resolver)
                    .ok()
                    .flatten()
                    .map_or_else(|| "unknown".to_string(), |ty| ty.regtype_name());
                return ScalarExpr::Cast {
                    expr: Box::new(ScalarExpr::Literal(Value::Str(name))),
                    ty: "regtype".into(),
                };
            }
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                filter,
            }
        }
        ScalarExpr::Array(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            bind_common_type_expressions(&mut items, schema, params, resolver);
            ScalarExpr::Array(items)
        }
        ScalarExpr::Row(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::Row(items)
        }
        ScalarExpr::Binary {
            op,
            mut lhs,
            mut rhs,
        } => {
            bind_type_introspection_in_place(lhs.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(rhs.as_mut(), schema, params, resolver);
            ScalarExpr::Binary { op, lhs, rhs }
        }
        ScalarExpr::UnaryMinus(mut expr) => {
            let source_type = scalar_type_inner(&expr, schema, params, resolver)
                .ok()
                .flatten()
                .and_then(|ty| unary_minus_result_type(&ty).ok());
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            if let Some(source_type) = source_type {
                let source_name = source_type.sql_name();
                if !matches!(expr.as_ref(), ScalarExpr::Cast { ty, .. } if ty.eq_ignore_ascii_case(&source_name))
                {
                    let inner = std::mem::replace(expr.as_mut(), ScalarExpr::Literal(Value::Null));
                    *expr = ScalarExpr::Cast {
                        expr: Box::new(inner),
                        ty: source_name,
                    };
                }
            }
            ScalarExpr::UnaryMinus(expr)
        }
        ScalarExpr::Not(mut inner) => {
            bind_type_introspection_in_place(inner.as_mut(), schema, params, resolver);
            ScalarExpr::Not(inner)
        }
        ScalarExpr::And(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::And(items)
        }
        ScalarExpr::Or(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::Or(items)
        }
        ScalarExpr::IsNull { mut expr, negated } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            ScalarExpr::IsNull { expr, negated }
        }
        ScalarExpr::Between {
            mut expr,
            mut low,
            mut high,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(low.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(high.as_mut(), schema, params, resolver);
            ScalarExpr::Between { expr, low, high }
        }
        ScalarExpr::InList {
            mut expr,
            mut list,
            negated,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            bind_type_introspection_items(&mut list, schema, params, resolver);
            ScalarExpr::InList {
                expr,
                list,
                negated,
            }
        }
        ScalarExpr::WindowCall {
            name,
            mut args,
            mut spec,
        } => {
            bind_type_introspection_items(&mut args, schema, params, resolver);
            bind_type_introspection_items(&mut spec.partition_by, schema, params, resolver);
            for order in &mut spec.order_by {
                bind_type_introspection_in_place(&mut order.expr, schema, params, resolver);
            }
            if let Some(frame) = spec.frame.as_mut() {
                bind_frame_bound(&mut frame.start, schema, params, resolver);
                bind_frame_bound(&mut frame.end, schema, params, resolver);
            }
            ScalarExpr::WindowCall { name, args, spec }
        }
        ScalarExpr::Case {
            mut base,
            mut when,
            mut else_branch,
        } => {
            if let Some(base) = base.as_deref_mut() {
                bind_type_introspection_in_place(base, schema, params, resolver);
            }
            for (condition, result) in &mut when {
                bind_type_introspection_in_place(condition, schema, params, resolver);
                bind_type_introspection_in_place(result, schema, params, resolver);
            }
            if let Some(else_branch) = else_branch.as_deref_mut() {
                bind_type_introspection_in_place(else_branch, schema, params, resolver);
            }
            if base.is_some() {
                let comparison_type = common_expression_type(
                    base.iter()
                        .map(Box::as_ref)
                        .chain(when.iter().map(|(condition, _)| condition)),
                    schema,
                    params,
                    resolver,
                );
                if let Some(comparison_type) = comparison_type {
                    if let Some(base) = base.as_deref_mut() {
                        bind_common_type_cast(base, &comparison_type, schema, params, resolver);
                    }
                    for (condition, _) in &mut when {
                        bind_common_type_cast(
                            condition,
                            &comparison_type,
                            schema,
                            params,
                            resolver,
                        );
                    }
                }
            }
            let result_type = common_expression_type(
                when.iter()
                    .map(|(_, result)| result)
                    .chain(else_branch.iter().map(Box::as_ref)),
                schema,
                params,
                resolver,
            );
            if let Some(result_type) = result_type {
                for (_, result) in &mut when {
                    bind_common_type_cast(result, &result_type, schema, params, resolver);
                }
                if let Some(else_branch) = else_branch.as_deref_mut() {
                    bind_common_type_cast(else_branch, &result_type, schema, params, resolver);
                }
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            }
        }
        ScalarExpr::Cast { mut expr, ty } => {
            let source_type = cast_requires_declared_source(&ty)
                .then(|| {
                    scalar_type_inner(&expr, schema, params, resolver)
                        .ok()
                        .flatten()
                })
                .flatten();
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            if let Some(source_type) = source_type {
                let source_name = source_type.sql_name();
                if !matches!(expr.as_ref(), ScalarExpr::Cast { ty, .. } if ty.eq_ignore_ascii_case(&source_name))
                {
                    let inner = std::mem::replace(expr.as_mut(), ScalarExpr::Literal(Value::Null));
                    *expr = ScalarExpr::Cast {
                        expr: Box::new(inner),
                        ty: source_name,
                    };
                }
            }
            ScalarExpr::Cast { expr, ty }
        }
        ScalarExpr::InSubquery {
            mut expr,
            subquery,
            negated,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            }
        }
        other => other,
    }
}

fn requires_type_introspection_binding(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            is_pg_typeof(name)
                || is_common_type_function(name)
                || to_hex::is_function(name)
                || containment::is_operator(name)
                || args.iter().any(requires_type_introspection_binding)
                || order_by
                    .iter()
                    .any(|order| requires_type_introspection_binding(&order.expr))
                || filter
                    .as_deref()
                    .is_some_and(requires_type_introspection_binding)
        }
        ScalarExpr::Array(_) | ScalarExpr::Case { .. } | ScalarExpr::UnaryMinus(_) => true,
        ScalarExpr::Row(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(requires_type_introspection_binding)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            requires_type_introspection_binding(lhs) || requires_type_introspection_binding(rhs)
        }
        ScalarExpr::Not(expression)
        | ScalarExpr::IsNull {
            expr: expression, ..
        } => requires_type_introspection_binding(expression),
        ScalarExpr::Between { expr, low, high } => {
            requires_type_introspection_binding(expr)
                || requires_type_introspection_binding(low)
                || requires_type_introspection_binding(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            requires_type_introspection_binding(expr)
                || list.iter().any(requires_type_introspection_binding)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(requires_type_introspection_binding)
                || spec
                    .partition_by
                    .iter()
                    .any(requires_type_introspection_binding)
                || spec
                    .order_by
                    .iter()
                    .any(|order| requires_type_introspection_binding(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_requires_type_introspection_binding(&frame.start)
                        || frame_bound_requires_type_introspection_binding(&frame.end)
                })
        }
        ScalarExpr::Cast { expr, ty } => {
            cast_requires_declared_source(ty) || requires_type_introspection_binding(expr)
        }
        ScalarExpr::InSubquery { expr, .. } => requires_type_introspection_binding(expr),
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn frame_bound_requires_type_introspection_binding(bound: &crate::ScalarFrameBound) -> bool {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            requires_type_introspection_binding(expression)
        }
        crate::ScalarFrameBound::UnboundedPreceding
        | crate::ScalarFrameBound::UnboundedFollowing
        | crate::ScalarFrameBound::CurrentRow => false,
    }
}

fn is_pg_typeof(name: &str) -> bool {
    name.eq_ignore_ascii_case("pg_typeof") || name.eq_ignore_ascii_case("pg_catalog.pg_typeof")
}

fn is_common_type_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "coalesce" | "greatest" | "least"
    )
}

fn bind_common_type_expressions(
    expressions: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let Some(target) = common_expression_type(expressions.iter(), schema, params, resolver) else {
        return;
    };
    for expression in expressions {
        bind_common_type_cast(expression, &target, schema, params, resolver);
    }
}

fn common_expression_type<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Option<ColumnType> {
    let mut common = None;
    let mut saw_expression = false;
    for expression in expressions {
        saw_expression = true;
        let expression_type =
            common_context_expression_type(expression, schema, params, resolver).ok()?;
        common = merge_optional_types(common, expression_type).ok()?;
    }
    saw_expression.then(|| common.unwrap_or(ColumnType::Text))
}

fn bind_common_type_cast(
    expression: &mut ScalarExpr,
    target: &ColumnType,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let target = base_type(target);
    let source = common_context_expression_type(expression, schema, params, resolver)
        .ok()
        .flatten();
    if source
        .as_ref()
        .is_some_and(|source| base_type(source) == target)
    {
        return;
    }
    let inner = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = ScalarExpr::Cast {
        expr: Box::new(inner),
        ty: target.sql_name(),
    };
}

fn bind_type_introspection_items(
    expressions: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    for expression in expressions {
        bind_type_introspection_in_place(expression, schema, params, resolver);
    }
}

fn bind_type_introspection_in_place(
    expression: &mut ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let owned = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = bind_type_introspection_inner(owned, schema, params, resolver);
}

fn cast_requires_declared_source(target: &str) -> bool {
    let mut target = target.trim().to_ascii_lowercase();
    while let Some(element) = target.strip_suffix("[]") {
        target = element.trim_end().to_string();
    }
    matches!(
        target.as_str(),
        "bytea" | "pg_catalog.bytea" | "oid" | "pg_catalog.oid" | "xid" | "pg_catalog.xid"
    )
}

fn bind_frame_bound(
    bound: &mut crate::ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            bind_type_introspection_in_place(expression.as_mut(), schema, params, resolver);
        }
        crate::ScalarFrameBound::UnboundedPreceding
        | crate::ScalarFrameBound::UnboundedFollowing
        | crate::ScalarFrameBound::CurrentRow => {}
    }
}

pub fn values_column_types(
    rows: &[Vec<ScalarExpr>],
    params: &[SQLParam],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let width = rows.first().map_or(0, Vec::len);
    let empty = RowSchema::default();
    let mut types = vec![None; width];
    for row in rows {
        if row.len() != width {
            return Err(SQLError::TypeMismatch(
                "VALUES lists must all be the same length".into(),
            ));
        }
        for (position, expression) in row.iter().enumerate() {
            types[position] = merge_optional_types(
                types[position].take(),
                common_context_expression_type(expression, &empty, params, None)?,
            )?;
        }
    }
    Ok(types
        .into_iter()
        .map(|ty| ty.or(Some(ColumnType::Text)))
        .collect())
}

/// Resolve an expression participating in `PostgreSQL`'s common-type selection. Bare string and NULL literals retain the parser's `unknown` type until the surrounding VALUES, set operation, CASE, or array context selects a concrete type.
pub fn common_context_expression_type(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        return Ok(None);
    }
    scalar_type_inner(expression, schema, params, resolver)
}

fn parameter_type(parameter: &SQLParam) -> Option<ColumnType> {
    match parameter {
        SQLParam::Scalar(value) => value_type(value),
        SQLParam::Vector(values) => u32::try_from(values.len()).ok().map(ColumnType::Vector),
        SQLParam::Tensor(values) => values
            .first()
            .and_then(|values| u32::try_from(values.len()).ok())
            .map(ColumnType::Tensor),
    }
}

fn value_type(value: &Value) -> Option<ColumnType> {
    match value {
        Value::Null | Value::Map(_) => None,
        Value::Row(_) | Value::Record(_) => Some(ColumnType::Record),
        Value::Bool(_) => Some(ColumnType::Boolean),
        Value::Int(value) if i32::try_from(*value).is_ok() => Some(ColumnType::Integer),
        Value::Int(_) => Some(ColumnType::BigInteger),
        Value::Float(_) => Some(ColumnType::DoublePrecision),
        Value::Decimal(_) => Some(ColumnType::Numeric {
            precision: None,
            scale: None,
        }),
        Value::Str(_) => Some(ColumnType::Text),
        Value::FixedChar(value) => u32::try_from(value.chars().count())
            .ok()
            .map(ColumnType::Character),
        Value::Bytes(_) => Some(ColumnType::Bytea),
        Value::Temporal(value) => Some(match value {
            uqa_core::TemporalValue::Date { .. } => ColumnType::Date,
            uqa_core::TemporalValue::Time { .. } => ColumnType::Time,
            uqa_core::TemporalValue::TimeTz { .. } => ColumnType::TimeTz,
            uqa_core::TemporalValue::Timestamp { .. } => ColumnType::Timestamp,
            uqa_core::TemporalValue::TimestampTz { .. } => ColumnType::TimestampTz,
            uqa_core::TemporalValue::Interval { .. } => ColumnType::Interval,
        }),
        Value::Json(_) => Some(ColumnType::Json),
        Value::JsonB(_) => Some(ColumnType::JsonB),
        Value::Array(array) => {
            let mut element = None;
            merge_array_element_types(array.elements(), &mut element)?;
            element.map(|element| ColumnType::Array(Box::new(element)))
        }
        Value::List(values) => {
            let mut element = None;
            for value in values {
                element = merge_optional_types(element, value_type(value)).ok()?;
            }
            element.map(|element| ColumnType::Array(Box::new(element)))
        }
    }
}

fn merge_array_element_types(values: &[Value], element: &mut Option<ColumnType>) -> Option<()> {
    for value in values {
        if let Value::List(nested) = value {
            merge_array_element_types(nested, element)?;
        } else {
            *element = merge_optional_types(element.take(), value_type(value)).ok()?;
        }
    }
    Some(())
}

fn merge_optional_types(
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) => common_type(&left, &right).map(Some),
    }
}

pub fn common_type(left: &ColumnType, right: &ColumnType) -> Result<ColumnType, SQLError> {
    if left == right {
        return Ok(left.clone());
    }
    if matches!(left, ColumnType::Domain { .. }) || matches!(right, ColumnType::Domain { .. }) {
        return common_type(base_type(left), base_type(right));
    }
    if let Some(numeric) = common_numeric_type(left, right) {
        return Ok(numeric);
    }
    if matches!(left, ColumnType::Oid) && is_integral_type(right)
        || matches!(right, ColumnType::Oid) && is_integral_type(left)
    {
        return Ok(ColumnType::Oid);
    }
    if left.is_character_string() && right.is_character_string() {
        return Ok(match left {
            ColumnType::Bpchar | ColumnType::Character(_) => ColumnType::Bpchar,
            ColumnType::Varchar(_) => ColumnType::Varchar(None),
            ColumnType::Name => ColumnType::Name,
            _ => ColumnType::Text,
        });
    }
    match (left, right) {
        (ColumnType::Date, ColumnType::Timestamp) | (ColumnType::Timestamp, ColumnType::Date) => {
            Ok(ColumnType::Timestamp)
        }
        (ColumnType::Date | ColumnType::Timestamp, ColumnType::TimestampTz)
        | (ColumnType::TimestampTz, ColumnType::Date | ColumnType::Timestamp) => {
            Ok(ColumnType::TimestampTz)
        }
        (ColumnType::Array(left), ColumnType::Array(right)) => {
            common_type(left, right).map(|element| ColumnType::Array(Box::new(element)))
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "types {} and {} cannot be matched",
            left.sql_name(),
            right.sql_name()
        ))),
    }
}

fn is_integral_type(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
    )
}

fn common_numeric_type(left: &ColumnType, right: &ColumnType) -> Option<ColumnType> {
    let rank = numeric_rank(left)?.max(numeric_rank(right)?);
    Some(match rank {
        0 => ColumnType::SmallInteger,
        1 => ColumnType::Integer,
        2 => ColumnType::BigInteger,
        3 => ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        4 => ColumnType::Real,
        _ => ColumnType::DoublePrecision,
    })
}

fn numeric_rank(ty: &ColumnType) -> Option<u8> {
    match ty {
        ColumnType::SmallInteger => Some(0),
        ColumnType::Integer => Some(1),
        ColumnType::BigInteger => Some(2),
        ColumnType::Numeric { .. } => Some(3),
        ColumnType::Real => Some(4),
        ColumnType::DoublePrecision => Some(5),
        _ => None,
    }
}

pub fn equality_operand_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, SQLError> {
    if matches!(left, ColumnType::Json) || matches!(right, ColumnType::Json) {
        return Err(undefined_equality_operator(left, right));
    }
    if matches!(left, ColumnType::Array(_)) || matches!(right, ColumnType::Array(_)) {
        if left == right {
            return Ok(left.clone());
        }
        return Err(undefined_equality_operator(left, right));
    }
    if left.is_character_string() && right.is_character_string() {
        if matches!(left, ColumnType::Bpchar | ColumnType::Character(_))
            || matches!(right, ColumnType::Bpchar | ColumnType::Character(_))
        {
            return Ok(ColumnType::Bpchar);
        }
        return Ok(ColumnType::Text);
    }
    common_type(left, right).map_err(|_| undefined_equality_operator(left, right))
}

fn undefined_equality_operator(left: &ColumnType, right: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} = {}",
            left.sql_name(),
            right.sql_name()
        ),
    }
}

#[cfg(test)]
mod tests;
