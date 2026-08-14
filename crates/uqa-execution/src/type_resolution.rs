//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL type propagation and PostgreSQL-compatible common-type rules.

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

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
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => Ok(schema.qualified_type(qualifier, column, key).cloned()),
        ScalarExpr::Literal(value) => Ok(value_type(value)),
        ScalarExpr::Param(index) => Ok(index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .and_then(parameter_type)),
        ScalarExpr::Cast { ty, .. } => ColumnType::from_sql_name(ty).map(Some),
        ScalarExpr::Array(items) => {
            let mut element = None;
            for item in items {
                element = merge_optional_types(
                    element,
                    scalar_type_inner(item, schema, params, resolver)?,
                )?;
            }
            Ok(element.map(|element| ColumnType::Array(Box::new(element))))
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = scalar_type_inner(lhs, schema, params, resolver)?;
            let right = scalar_type_inner(rhs, schema, params, resolver)?;
            binary_result_type(*op, left.as_ref(), right.as_ref())
        }
        ScalarExpr::UnaryMinus(inner) => scalar_type_inner(inner, schema, params, resolver)?
            .map_or(Ok(None), |ty| unary_minus_result_type(&ty).map(Some)),
        ScalarExpr::Not(_)
        | ScalarExpr::And(_)
        | ScalarExpr::Or(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Between { .. }
        | ScalarExpr::InList { .. }
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => Ok(Some(ColumnType::Boolean)),
        ScalarExpr::Case {
            when, else_branch, ..
        } => {
            let mut result = None;
            for (_, value) in when {
                result = merge_optional_types(
                    result,
                    scalar_type_inner(value, schema, params, resolver)?,
                )?;
            }
            if let Some(value) = else_branch {
                result = merge_optional_types(
                    result,
                    scalar_type_inner(value, schema, params, resolver)?,
                )?;
            }
            Ok(result)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            ..
        } => builtin_function_type_inner(
            name,
            binding.as_ref(),
            args,
            order_by,
            schema,
            params,
            resolver,
        ),
        ScalarExpr::WindowCall { name, args, .. } => {
            builtin_function_type_inner(name, None, args, &[], schema, params, resolver)
        }
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Star | ScalarExpr::Default => Ok(None),
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
        | "to_hex"
        | "quote_ident"
        | "quote_literal"
        | "quote_nullable"
        | "regexp_substr"
        | "array_to_string"
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
        "count" | "row_number" | "rank" | "dense_rank" | "crc32" | "crc32c" | "nextval"
        | "currval" | "setval" => Ok(Some(ColumnType::BigInteger)),
        "ntile" => Ok(Some(ColumnType::Integer)),
        "sum" => Ok(first()?.and_then(|ty| aggregate_sum_type(&ty))),
        "avg" => Ok(first()?.and_then(|ty| aggregate_average_type(&ty))),
        "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
            Ok(first()?.and_then(|ty| aggregate_average_type(&ty)))
        }
        "min" | "max" | "lag" | "lead" | "first_value" | "last_value" | "nth_value" | "nullif"
        | "array_cat" | "array_remove" | "array_replace" | "trim_array" | "array_sample"
        | "array_reverse" | "array_sort" | "__slice" | "array_append" | "generate_series" => {
            first()
        }
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
        "coalesce" | "greatest" | "least" => common_argument_type(args, schema, params, resolver),
        "reverse" => Ok(first()?.map(|ty| {
            if matches!(base_type(&ty), ColumnType::Bytea) {
                ColumnType::Bytea
            } else {
                ColumnType::Text
            }
        })),
        "concat_op" => concat_type(argument(0)?, argument(1)?),
        "length" | "char_length" | "character_length" | "octet_length" | "position" | "strpos"
        | "ascii" | "width_bucket" | "bit_length" | "regexp_count" | "regexp_instr"
        | "num_nulls" | "num_nonnulls" | "array_length" | "array_upper" | "array_lower"
        | "cardinality" | "array_position" | "json_array_length" | "jsonb_array_length" => {
            Ok(Some(ColumnType::Integer))
        }
        "abs" => Ok(first()?.map(|ty| base_type(&ty).clone())),
        "round" | "trunc" | "ceil" | "ceiling" | "floor" | "sign" => {
            Ok(first()?.map(|ty| numeric_unary_result_type(&ty)))
        }
        "mod" | "gcd" | "lcm" => numeric_binary_function_type(argument(0)?, argument(1)?),
        "div" | "factorial" | "extract" | "to_number" => Ok(Some(numeric_type())),
        "power" | "pow" | "sqrt" | "ln" | "log" | "log10" => {
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
        "__subscript" | "unnest" => Ok(first()?.and_then(array_element_type)),
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
        argument_types.push(scalar_type_inner(value, schema, params, Some(resolver))?);
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
        ScalarExpr::Literal(Value::Str(name)) => Some(name.to_ascii_lowercase()),
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
            scalar_type_inner(named_argument_value(argument), schema, params, resolver)?,
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

/// Bind polymorphic type-introspection calls while the input schema still
/// carries declared SQL types. Runtime values deliberately do not encode
/// integer widths, varchar identity, or float widths.
pub fn bind_type_introspection(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> ScalarExpr {
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let args = args
                .into_iter()
                .map(|argument| bind_type_introspection(argument, schema, params))
                .collect::<Vec<_>>();
            if name.eq_ignore_ascii_case("pg_typeof") && args.len() == 1 {
                let name = scalar_type(&args[0], schema, params)
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
                order_by: order_by
                    .into_iter()
                    .map(|mut order| {
                        order.expr = bind_type_introspection(order.expr, schema, params);
                        order
                    })
                    .collect(),
                filter: filter
                    .map(|filter| Box::new(bind_type_introspection(*filter, schema, params))),
            }
        }
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .into_iter()
                .map(|item| bind_type_introspection(item, schema, params))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op,
            lhs: Box::new(bind_type_introspection(*lhs, schema, params)),
            rhs: Box::new(bind_type_introspection(*rhs, schema, params)),
        },
        ScalarExpr::UnaryMinus(expr) => {
            let source_type = scalar_type(&expr, schema, params)
                .ok()
                .flatten()
                .and_then(|ty| unary_minus_result_type(&ty).ok());
            let mut expr = bind_type_introspection(*expr, schema, params);
            if let Some(source_type) = source_type {
                let source_name = source_type.sql_name();
                if !matches!(&expr, ScalarExpr::Cast { ty, .. } if ty.eq_ignore_ascii_case(&source_name))
                {
                    expr = ScalarExpr::Cast {
                        expr: Box::new(expr),
                        ty: source_name,
                    };
                }
            }
            ScalarExpr::UnaryMinus(Box::new(expr))
        }
        ScalarExpr::Not(inner) => {
            ScalarExpr::Not(Box::new(bind_type_introspection(*inner, schema, params)))
        }
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .into_iter()
                .map(|item| bind_type_introspection(item, schema, params))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .into_iter()
                .map(|item| bind_type_introspection(item, schema, params))
                .collect(),
        ),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(bind_type_introspection(*expr, schema, params)),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(bind_type_introspection(*expr, schema, params)),
            low: Box::new(bind_type_introspection(*low, schema, params)),
            high: Box::new(bind_type_introspection(*high, schema, params)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(bind_type_introspection(*expr, schema, params)),
            list: list
                .into_iter()
                .map(|item| bind_type_introspection(item, schema, params))
                .collect(),
            negated,
        },
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
        } => {
            spec.partition_by = spec
                .partition_by
                .into_iter()
                .map(|item| bind_type_introspection(item, schema, params))
                .collect();
            spec.order_by = spec
                .order_by
                .into_iter()
                .map(|mut order| {
                    order.expr = bind_type_introspection(order.expr, schema, params);
                    order
                })
                .collect();
            if let Some(frame) = spec.frame.as_mut() {
                bind_frame_bound(&mut frame.start, schema, params);
                bind_frame_bound(&mut frame.end, schema, params);
            }
            ScalarExpr::WindowCall {
                name,
                args: args
                    .into_iter()
                    .map(|item| bind_type_introspection(item, schema, params))
                    .collect(),
                spec,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.map(|base| Box::new(bind_type_introspection(*base, schema, params))),
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    (
                        bind_type_introspection(condition, schema, params),
                        bind_type_introspection(result, schema, params),
                    )
                })
                .collect(),
            else_branch: else_branch
                .map(|branch| Box::new(bind_type_introspection(*branch, schema, params))),
        },
        ScalarExpr::Cast { expr, ty } => {
            let source_type = cast_requires_declared_source(&ty)
                .then(|| scalar_type(&expr, schema, params).ok().flatten())
                .flatten();
            let mut expr = bind_type_introspection(*expr, schema, params);
            if let Some(source_type) = source_type {
                let source_name = source_type.sql_name();
                if !matches!(&expr, ScalarExpr::Cast { ty, .. } if ty.eq_ignore_ascii_case(&source_name))
                {
                    expr = ScalarExpr::Cast {
                        expr: Box::new(expr),
                        ty: source_name,
                    };
                }
            }
            ScalarExpr::Cast {
                expr: Box::new(expr),
                ty,
            }
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(bind_type_introspection(*expr, schema, params)),
            subquery,
            negated,
        },
        other => other,
    }
}

fn cast_requires_declared_source(target: &str) -> bool {
    matches!(
        target.trim().to_ascii_lowercase().as_str(),
        "bytea" | "pg_catalog.bytea" | "oid" | "pg_catalog.oid" | "xid" | "pg_catalog.xid"
    )
}

fn bind_frame_bound(bound: &mut crate::ScalarFrameBound, schema: &RowSchema, params: &[SQLParam]) {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            let bound = std::mem::replace(expression, Box::new(ScalarExpr::Literal(Value::Null)));
            *expression = Box::new(bind_type_introspection(*bound, schema, params));
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
                scalar_type(expression, &empty, params)?,
            )?;
        }
    }
    Ok(types
        .into_iter()
        .map(|ty| ty.or(Some(ColumnType::Text)))
        .collect())
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
        Value::List(values) => {
            let mut element = None;
            for value in values {
                element = merge_optional_types(element, value_type(value)).ok()?;
            }
            element.map(|element| ColumnType::Array(Box::new(element)))
        }
    }
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
mod tests {
    use super::*;

    #[test]
    fn common_type_matches_postgresql_numeric_and_left_character_precedence() {
        assert_eq!(
            common_type(&ColumnType::SmallInteger, &ColumnType::BigInteger).unwrap(),
            ColumnType::BigInteger
        );
        assert_eq!(
            common_type(
                &ColumnType::Numeric {
                    precision: None,
                    scale: None,
                },
                &ColumnType::Real,
            )
            .unwrap(),
            ColumnType::Real
        );
        assert_eq!(
            common_type(&ColumnType::Varchar(Some(8)), &ColumnType::Text).unwrap(),
            ColumnType::Varchar(None)
        );
        assert_eq!(
            common_type(&ColumnType::Text, &ColumnType::Varchar(Some(8))).unwrap(),
            ColumnType::Text
        );
    }

    #[test]
    fn equality_resolution_rejects_postgresql_undefined_operators() {
        for (left, right) in [
            (ColumnType::Boolean, ColumnType::Integer),
            (ColumnType::Json, ColumnType::Json),
            (
                ColumnType::Array(Box::new(ColumnType::Integer)),
                ColumnType::Array(Box::new(ColumnType::BigInteger)),
            ),
        ] {
            let error = equality_operand_type(&left, &right).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42883"));
        }
    }

    #[test]
    fn values_type_resolution_uses_declared_casts_instead_of_runtime_values() {
        let rows = vec![
            vec![ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
                ty: "smallint".into(),
            }],
            vec![ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Int(2))),
                ty: "bigint".into(),
            }],
        ];
        assert_eq!(
            values_column_types(&rows, &[]).unwrap(),
            vec![Some(ColumnType::BigInteger)]
        );
    }

    #[test]
    fn type_introspection_binds_before_integer_width_is_erased() {
        let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::SmallInteger)]);
        let expression = ScalarExpr::Func {
            name: "pg_typeof".into(),
            binding: None,
            args: vec![ScalarExpr::Column("v".into())],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        assert_eq!(
            bind_type_introspection(expression, &schema, &[]),
            ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Str("smallint".into()))),
                ty: "regtype".into(),
            }
        );
    }
}
