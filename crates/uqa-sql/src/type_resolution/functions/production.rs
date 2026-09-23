//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in result inference retains type buffers and selected payloads under its caller's control.

use super::super::common::{
    base_type, common_numeric_type, common_type_with_control, numeric_type,
};
use super::super::{
    array_transform,
    call::{infer_with_control, InferType},
    containment, fixed_builtin, range, FunctionTypeResolver,
};
use super::FunctionTypeCall;
use super::{named_argument, named_argument_value};
use crate::{
    ast::{BinaryOp, ColumnType, FunctionBinding, FunctionDispatch},
    SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

fn lowercase(name: &str, control: &ProductionControl<'_>) -> Result<Produced<String>, SQLError> {
    let mut output = ProductionString::new(*control);
    output.reserve(name.len())?;
    for character in name.chars() {
        output.push(character.to_ascii_lowercase())?;
    }
    Ok(output.finish()?)
}

fn inline(
    ty: ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    Ok(Some(control.finish(ty, control.empty_reservation())?))
}

fn optional_inline(
    ty: Option<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    ty.map(|ty| {
        control
            .finish(ty, control.empty_reservation())
            .map_err(Into::into)
    })
    .transpose()
}

fn copy(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    ty.map(|ty| ty.clone_with_control(control).map_err(Into::into))
        .transpose()
}

fn array(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    ty.map(|ty| {
        ColumnType::array_with_control(ty.clone_with_control(control)?, control).map_err(Into::into)
    })
    .transpose()
}

fn array_element_type(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match ty {
        Some(ColumnType::Array(element)) => copy(Some(element), control),
        Some(ColumnType::Int2Vector) => inline(ColumnType::SmallInteger, control),
        Some(ColumnType::OidVector) => inline(ColumnType::Oid, control),
        _ => Ok(None),
    }
}

fn push_type(
    output: &mut ProductionVec<'_, Option<ColumnType>>,
    ty: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let value = match ty {
        Some(ty) => {
            let (ty, memory) = ty.into_parts();
            control.finish(Some(ty), memory)?
        }
        None => control.finish(None, control.empty_reservation())?,
    };
    Ok(output.push_produced(value)?)
}

fn infer_types<'a>(
    expressions: impl Iterator<Item = &'a ScalarExpr>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(expressions.size_hint().0)?;
    for expression in expressions {
        push_type(
            &mut output,
            infer_with_control(expression, infer, control)?,
            control,
        )?;
    }
    Ok(output.finish()?)
}

fn numeric_operands(
    binding: &FunctionBinding,
    args: &[ScalarExpr],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    if binding.argument_types.is_empty() {
        output.reserve(args.len())?;
        for argument in args {
            let ty = if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                None
            } else {
                infer_with_control(argument, infer, control)?
            };
            push_type(&mut output, ty, control)?;
        }
    } else {
        output.reserve(binding.argument_types.len())?;
        for name in &binding.argument_types {
            push_type(
                &mut output,
                Some(ColumnType::from_sql_name_with_control(name, control)?),
                control,
            )?;
        }
    }
    Ok(output.finish()?)
}

fn fixed_type(
    call: FunctionTypeCall<'_>,
    types: &[Option<ColumnType>],
    explicit: bool,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let FunctionTypeCall {
        name,
        binding,
        args,
    } = call;
    if resolver.is_some() {
        // External catalog callbacks keep their established ordinary protocol; retained inference has no resolver, enforced at the shared entry.
        return optional_inline(
            fixed_builtin::resolve_type(name, binding, args, types, explicit, params, resolver)?,
            control,
        );
    }
    let mut names = ProductionVec::new(*control);
    names.reserve(args.len())?;
    let mut effective = ProductionVec::new(*control);
    effective.reserve(args.len())?;
    for (argument, ty) in args.iter().zip(types) {
        let argument = crate::scalar_call_argument(argument)?;
        let name = match argument.name {
            Some(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                control.finish(Some(name), memory)?
            }
            None => control.finish(None, control.empty_reservation())?,
        };
        names.push_produced(name)?;
        let ty = super::super::common::effective_overload_argument_type_ref_with_params(
            argument.value,
            ty.as_ref(),
            params,
        );
        push_type(&mut effective, copy(ty, control)?, control)?;
    }
    let names = names.finish()?;
    let effective = effective.finish()?;
    fixed_builtin::resolve_return_type_with_control(
        name, binding, &names, &effective, explicit, control,
    )
    .map(Some)
}

fn array_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    types: &[Option<ColumnType>],
    explicit: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if resolver.is_some() {
        return optional_inline(
            array_transform::resolve_type(name, binding, args, types, explicit, resolver)?,
            control,
        );
    }
    array_transform::resolve_type_with_control(name, binding, args, types, explicit, control)
}

fn extension_type(
    resolver: Option<&dyn FunctionTypeResolver>,
    call: FunctionTypeCall<'_>,
    resolved_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let FunctionTypeCall {
        name,
        binding,
        args,
    } = call;
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    assert!(
        control.budget().is_none(),
        "catalog callback is ordinary-only"
    );
    let mut names = Vec::with_capacity(args.len());
    let mut types = Vec::with_capacity(args.len());
    for (argument, ty) in args.iter().zip(resolved_types) {
        let (name, value) = named_argument(argument);
        names.push(name);
        types.push(super::super::effective_overload_argument_type_with_params(
            value,
            ty.clone(),
            params,
        ));
    }
    optional_inline(
        resolver.resolve_function_type(name, binding, &names, &types, explicit_variadic)?,
        control,
    )
}

fn common_argument_type(
    args: &[ScalarExpr],
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let mut result: Option<Produced<ColumnType>> = None;
    for (argument, ty) in args.iter().zip(types) {
        control.check()?;
        if matches!(
            named_argument_value(argument),
            ScalarExpr::Literal(Value::Str(_) | Value::Null)
        ) {
            continue;
        }
        if let Some(ty) = ty {
            result = Some(match &result {
                Some(existing) => common_type_with_control(existing, ty, control)?,
                None => ty.clone_with_control(control)?,
            });
        }
    }
    match result {
        Some(result) => Ok(Some(result)),
        None => inline(ColumnType::Text, control),
    }
}

fn concat_type(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match (left, right) {
        (Some(ColumnType::Array(left)), Some(ColumnType::Array(right))) => {
            Ok(Some(ColumnType::array_with_control(
                common_type_with_control(left, right, control)?,
                control,
            )?))
        }
        (Some(array @ ColumnType::Array(_)), _) | (_, Some(array @ ColumnType::Array(_))) => {
            copy(Some(array), control)
        }
        (Some(ColumnType::JsonB), Some(ColumnType::JsonB)) => inline(ColumnType::JsonB, control),
        _ => inline(ColumnType::Text, control),
    }
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

fn numeric_unary_result_type(ty: &ColumnType) -> ColumnType {
    if matches!(base_type(ty), ColumnType::Numeric { .. }) {
        numeric_type()
    } else {
        ColumnType::DoublePrecision
    }
}

fn numeric_binary_function_type(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match (left, right) {
        (Some(left), Some(right)) => inline(
            common_numeric_type(base_type(left), base_type(right)).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "types {} and {} are not numeric",
                    left.sql_name(),
                    right.sql_name()
                ))
            })?,
            control,
        ),
        (Some(ty), None) | (None, Some(ty)) => copy(Some(base_type(ty)), control),
        (None, None) => Ok(None),
    }
}

fn numeric_transcendental_type(
    args: &[ScalarExpr],
    types: &[Option<ColumnType>],
) -> Option<ColumnType> {
    let mut saw_argument = false;
    for ty in types.iter().flatten() {
        saw_argument = true;
        if !matches!(base_type(ty), ColumnType::Numeric { .. }) {
            return Some(ColumnType::DoublePrecision);
        }
    }
    if saw_argument {
        Some(numeric_type())
    } else if args.is_empty() {
        None
    } else {
        Some(ColumnType::DoublePrecision)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
pub(in crate::type_resolution) fn builtin_function_type_with_control(
    call: FunctionTypeCall<'_>,
    order_by: &[crate::ScalarOrder],
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let FunctionTypeCall {
        name,
        binding,
        args,
    } = call;
    control.check()?;
    assert!(
        resolver.is_none() || control.budget().is_none(),
        "catalog callbacks are resolved outside retained generated inference"
    );
    if let Some((binding, FunctionDispatch::NumericOperator(operator))) =
        binding.and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
    {
        if let Some(error) = &binding.resolution_error {
            return Err(error.sql_error());
        }
        let operand_types = numeric_operands(binding, args, infer, control)?;
        let selected = super::super::operators::numeric_operator_types_with_control(
            operator,
            &operand_types,
            control,
        )?;
        return copy(Some(&selected.result), control);
    }

    let original_name = name;
    let lower = lowercase(name, control)?;
    let name = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if binding.is_none()
        && resolver.is_some_and(|resolver| resolver.has_untyped_function(original_name))
    {
        return Ok(None);
    }
    if name.contains('.')
        && resolver.is_none()
        && binding.and_then(|binding| binding.dispatch).is_none()
    {
        return Ok(None);
    }
    let call_arguments = crate::ir::scalar_call_arguments_with_control(args, control)?;
    let explicit_variadic = call_arguments
        .iter()
        .any(|argument| argument.explicit_variadic);
    let argument_types = infer_types(
        call_arguments.iter().map(|argument| argument.value),
        infer,
        control,
    )?;
    if name.contains('.') && binding.and_then(|binding| binding.dispatch).is_none() {
        return extension_type(
            resolver,
            FunctionTypeCall {
                name: original_name,
                binding,
                args,
            },
            &argument_types,
            explicit_variadic,
            params,
            control,
        );
    }
    let ordered_argument_types =
        infer_types(order_by.iter().map(|order| &order.expr), infer, control)?;
    let argument = |position: usize| argument_types.get(position).and_then(Option::as_ref);
    let ordered_argument = || ordered_argument_types.first().and_then(Option::as_ref);
    let first = || argument(0);
    if let Some(dispatch) = binding.and_then(|binding| binding.dispatch) {
        match dispatch {
            FunctionDispatch::NumericOperator(_) => unreachable!("numeric operator handled above"),
            FunctionDispatch::JsonExtract { as_text, .. } => {
                let input = first();
                let input = input.map(base_type);
                return match input {
                    Some(ColumnType::Json | ColumnType::JsonB) => {
                        if as_text {
                            inline(ColumnType::Text, control)
                        } else {
                            copy(input, control)
                        }
                    }
                    None => Ok(None),
                    Some(other) => Err(SQLError::Routine {
                        sqlstate: "42883".into(),
                        message: format!(
                            "JSON extraction operator does not exist for {}",
                            other.sql_name()
                        ),
                    }),
                };
            }
            FunctionDispatch::NamedArgument | FunctionDispatch::VariadicArgument => {
                return copy(first(), control);
            }
            FunctionDispatch::ArraySubscripts | FunctionDispatch::Subscript => {
                return array_element_type(first(), control);
            }
            FunctionDispatch::ArraySlices | FunctionDispatch::Slice => {
                return copy(first(), control)
            }
            FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator => {
                let operator = match args.get(2) {
                    Some(ScalarExpr::Literal(Value::Str(operator))) => match operator.as_str() {
                        "=" => Some(BinaryOp::Equal),
                        "<>" | "!=" => Some(BinaryOp::NotEqual),
                        "<" => Some(BinaryOp::Less),
                        "<=" => Some(BinaryOp::LessEqual),
                        ">" => Some(BinaryOp::Greater),
                        ">=" => Some(BinaryOp::GreaterEqual),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(operator) = operator {
                    super::super::operators::binary_result_type_with_control(
                        operator,
                        argument(0),
                        None,
                        control,
                    )?;
                }
                return inline(ColumnType::Boolean, control);
            }
            FunctionDispatch::IsDistinct => {
                super::super::operators::binary_result_type_with_control(
                    BinaryOp::Equal,
                    argument(0),
                    argument(1),
                    control,
                )?;
                return inline(ColumnType::Boolean, control);
            }
            FunctionDispatch::BetweenSymmetric => {
                let value = argument(0);
                for bound in [argument(1), argument(2)] {
                    super::super::operators::binary_result_type_with_control(
                        BinaryOp::GreaterEqual,
                        value,
                        bound,
                        control,
                    )?;
                }
                return inline(ColumnType::Boolean, control);
            }
            FunctionDispatch::ToBinInt4
            | FunctionDispatch::ToBinInt8
            | FunctionDispatch::ToHexInt4
            | FunctionDispatch::ToHexInt8
            | FunctionDispatch::ToOctInt4
            | FunctionDispatch::ToOctInt8
            | FunctionDispatch::RandomInt4Range
            | FunctionDispatch::RandomInt8Range
            | FunctionDispatch::RandomNumericRange
            | FunctionDispatch::ArraySortJson
            | FunctionDispatch::Range { .. } => {}
        }
    }
    if let Some(ty) = range::function_type(name, binding, &argument_types) {
        return inline(ty, control);
    }
    if fixed_builtin::is_function(name) {
        return fixed_type(
            FunctionTypeCall {
                name: original_name,
                binding,
                args,
            },
            &argument_types,
            explicit_variadic,
            params,
            resolver,
            control,
        );
    }
    match name {
        "pg_typeof" => inline(ColumnType::Regtype, control),
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
        | "jsonb_extract_path_text" => inline(ColumnType::Text, control),
        "array_sort" | "array_reverse" => array_type(
            original_name,
            binding,
            args,
            &argument_types,
            explicit_variadic,
            resolver,
            control,
        ),
        "count" | "row_number" | "rank" | "dense_rank" | "nextval" | "currval" | "lastval"
        | "setval" => inline(ColumnType::BigInteger, control),
        "sum" => optional_inline(first().and_then(aggregate_sum_type), control),
        "avg" => optional_inline(first().and_then(aggregate_average_type), control),
        "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
            optional_inline(first().and_then(aggregate_average_type), control)
        }
        "min" | "max" | "lag" | "lead" | "first_value" | "last_value" | "nth_value" | "nullif"
        | "array_cat" | "array_remove" | "array_replace" | "trim_array" | "array_sample"
        | "array_append" | "generate_series" => copy(first(), control),
        "mode" | "percentile_disc" => copy(ordered_argument(), control),
        "percentile_cont" => optional_inline(
            ordered_argument().map(|ty| match base_type(ty) {
                ColumnType::Interval => ColumnType::Interval,
                _ => ColumnType::DoublePrecision,
            }),
            control,
        ),
        "array_agg" => array(first(), control),
        "string_agg" => optional_inline(
            first().map(|ty| {
                if matches!(ty, ColumnType::Bytea) {
                    ColumnType::Bytea
                } else {
                    ColumnType::Text
                }
            }),
            control,
        ),
        "json_agg"
        | "json_object_agg"
        | "json_array_elements"
        | "json_extract_path"
        | "to_json"
        | "row_to_json"
        | "json_build_object"
        | "json_build_array" => inline(ColumnType::Json, control),
        "jsonb_agg"
        | "jsonb_object_agg"
        | "jsonb_array_elements"
        | "jsonb_extract_path"
        | "json_delete_path"
        | "jsonb_set"
        | "jsonb_insert"
        | "to_jsonb"
        | "jsonb_build_object"
        | "jsonb_build_array" => inline(ColumnType::JsonB, control),
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            inline(ColumnType::Record, control)
        }
        "contains_op" | "contained_by_op" => {
            containment::resolve_operator_type_with_control(name, args, &argument_types, control)
        }
        "bool_and" | "bool_or" | "every" | "starts_with" | "like" | "ilike" | "similar_to"
        | "regexp_like" | "isfinite" | "json_contains" | "json_contained_by" | "json_has_key"
        | "json_has_any_key" | "json_has_all_keys" | "jsonb_path_exists" | "jsonpath_exists"
        | "jsonb_path_match" | "jsonpath_match" | "array_overlap" | "st_within" | "st_dwithin"
        | "overlaps" => inline(ColumnType::Boolean, control),
        "coalesce" | "greatest" | "least" => common_argument_type(args, &argument_types, control),
        "concat_op" => concat_type(argument(0), argument(1), control),
        "ntile" | "position" | "strpos" | "ascii" | "width_bucket" | "regexp_count"
        | "regexp_instr" | "num_nulls" | "num_nonnulls" | "array_length" | "array_upper"
        | "array_lower" | "array_ndims" | "cardinality" | "array_position"
        | "json_array_length" | "jsonb_array_length" => inline(ColumnType::Integer, control),
        "abs" => copy(first().map(base_type), control),
        "round" | "trunc" | "ceil" | "ceiling" | "floor" | "sign" => {
            optional_inline(first().map(numeric_unary_result_type), control)
        }
        "gcd" | "lcm" => numeric_binary_function_type(argument(0), argument(1), control),
        "div" | "factorial" | "extract" | "to_number" => inline(numeric_type(), control),
        "ln" | "log" | "log10" => {
            optional_inline(numeric_transcendental_type(args, &argument_types), control)
        }
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh"
        | "exp" | "log2" | "degrees" | "radians" | "pi" | "st_distance" | "date_part" => {
            inline(ColumnType::DoublePrecision, control)
        }
        "regexp_match" | "regexp_matches" | "string_to_array" => {
            array(Some(&ColumnType::Text), control)
        }
        "array_positions" => array(Some(&ColumnType::Integer), control),
        "decode" => inline(ColumnType::Bytea, control),
        "array_prepend" => copy(argument(1), control),
        "array_fill" => array(first(), control),
        "unnest" => array_element_type(first(), control),
        "now"
        | "current_timestamp"
        | "clock_timestamp"
        | "statement_timestamp"
        | "transaction_timestamp"
        | "to_timestamp" => inline(ColumnType::TimestampTz, control),
        "current_time" => inline(ColumnType::TimeTz, control),
        "localtime" => inline(ColumnType::Time, control),
        "localtimestamp" | "make_timestamp" => inline(ColumnType::Timestamp, control),
        "current_date" | "make_date" | "to_date" => inline(ColumnType::Date, control),
        "age" | "make_interval" | "justify_hours" => inline(ColumnType::Interval, control),
        "date_trunc" => optional_inline(
            argument(1).map(|ty| match base_type(ty) {
                ColumnType::Interval => ColumnType::Interval,
                ColumnType::Timestamp => ColumnType::Timestamp,
                _ => ColumnType::TimestampTz,
            }),
            control,
        ),
        "current_database" | "current_catalog" | "current_schema" | "current_user"
        | "session_user" => inline(ColumnType::Name, control),
        "current_schemas" => array(Some(&ColumnType::Name), control),
        _ => extension_type(
            resolver,
            FunctionTypeCall {
                name: original_name,
                binding,
                args,
            },
            &argument_types,
            explicit_variadic,
            params,
            control,
        ),
    }
}

#[cfg(test)]
mod tests;
