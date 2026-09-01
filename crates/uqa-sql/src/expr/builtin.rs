//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in strictness policy and structurally bound execution.

use uqa_core::Value;

use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::error::{Result, SQLError};

use super::call_arguments::normalized_function_name;
use super::call_dispatch::eval_builtin_function_call;
use super::context::EvalContext;
use super::{random, scalar_array, scalar_postgres, scalar_range};

pub fn builtin_scalar_function_strictness(name: &str, argument_count: usize) -> Option<bool> {
    let normalized = normalized_function_name(name);
    match normalized.as_ref() {
        "int4range" | "int8range" | "numrange" | "daterange" | "tsrange" | "tstzrange"
            if matches!(argument_count, 2 | 3) =>
        {
            Some(false)
        }
        "int4multirange" | "int8multirange" | "nummultirange" | "datemultirange"
        | "tsmultirange" | "tstzmultirange"
            if argument_count <= 1 =>
        {
            Some(true)
        }
        "multirange" if argument_count == 1 => Some(true),
        "coalesce" | "greatest" | "least" if argument_count >= 1 => Some(false),
        "nullif" | "concat_op" if argument_count == 2 => Some(false),
        "concat" | "format" | "json_build_array" | "jsonb_build_array" | "json_build_object"
        | "jsonb_build_object" | "num_nulls" | "num_nonnulls" => Some(false),
        "concat_ws" if argument_count >= 1 => Some(false),
        "quote_nullable" | "pg_typeof" | "typeof" if argument_count == 1 => Some(false),
        "array_cat" | "array_append" | "array_prepend" | "array_remove" | "array_positions"
            if argument_count == 2 =>
        {
            Some(false)
        }
        "array_position" if matches!(argument_count, 2 | 3) => Some(false),
        "array_replace" if argument_count == 3 => Some(false),
        "array_fill" if matches!(argument_count, 2 | 3) => Some(false),
        "array_to_string" if argument_count == 3 => Some(false),
        "string_to_array" | "string_to_table" if matches!(argument_count, 2 | 3) => Some(false),
        "pg_has_role" if matches!(argument_count, 2 | 3) => Some(true),
        "overlaps" if argument_count == 4 => Some(false),
        "abs"
        | "acos"
        | "array_dims"
        | "array_ndims"
        | "array_reverse"
        | "ascii"
        | "asin"
        | "atan"
        | "bit_length"
        | "cardinality"
        | "casefold"
        | "cbrt"
        | "ceil"
        | "ceiling"
        | "char_length"
        | "character_length"
        | "chr"
        | "cos"
        | "cosh"
        | "current_schemas"
        | "degrees"
        | "exp"
        | "factorial"
        | "floor"
        | "gamma"
        | "initcap"
        | "isfinite"
        | "json_array_length"
        | "jsonb_array_length"
        | "json_typeof"
        | "jsonb_typeof"
        | "jsonb_pretty"
        | "justify_hours"
        | "length"
        | "lgamma"
        | "ln"
        | "log10"
        | "log2"
        | "lower"
        | "md5"
        | "octet_length"
        | "quote_ident"
        | "quote_literal"
        | "radians"
        | "reverse"
        | "row_to_json"
        | "sign"
        | "sin"
        | "sinh"
        | "sqrt"
        | "tan"
        | "tanh"
        | "to_bin"
        | "to_hex"
        | "to_oct"
        | "to_json"
        | "to_jsonb"
        | "to_regclass"
        | "to_timestamp"
        | "upper"
        | "uuid_extract_timestamp"
        | "uuid_extract_version"
            if argument_count == 1 =>
        {
            Some(true)
        }
        "random" if argument_count == 2 => Some(true),
        "age" | "btrim" | "ltrim" | "rtrim" | "trim" | "log" | "round" | "trunc"
        | "json_strip_nulls" | "jsonb_strip_nulls"
            if matches!(argument_count, 1 | 2) =>
        {
            Some(true)
        }
        "array_sort" if matches!(argument_count, 1..=3) => Some(true),
        "array_length" | "array_lower" | "array_upper" | "atan2" | "date_part" | "date_trunc"
        | "decode" | "encode" | "extract" | "gcd" | "lcm" | "left" | "mod" | "power" | "pow"
        | "repeat" | "right" | "starts_with" | "position" | "strpos" | "to_char" | "to_date"
        | "to_number" | "trim_array" | "point" | "st_distance" | "st_within"
            if argument_count == 2 =>
        {
            Some(true)
        }
        "like" | "ilike" | "similar_to" if argument_count == 2 => Some(true),
        "like" | "ilike" | "similar_to" if argument_count == 3 => Some(false),
        "array_to_string" if argument_count == 2 => Some(true),
        "substring" | "substr" | "lpad" | "rpad" if matches!(argument_count, 2 | 3) => Some(true),
        "regexp_count" if matches!(argument_count, 2..=4) => Some(true),
        "regexp_instr" if matches!(argument_count, 2..=7) => Some(true),
        "regexp_like" | "regexp_match" | "regexp_matches" if matches!(argument_count, 2 | 3) => {
            Some(true)
        }
        "regexp_replace" if matches!(argument_count, 3..=6) => Some(true),
        "regexp_substr" if matches!(argument_count, 2..=6) => Some(true),
        "replace" | "split_part" | "translate" | "make_date" if argument_count == 3 => Some(true),
        "overlay" | "jsonb_set" | "jsonb_insert" if matches!(argument_count, 3 | 4) => Some(true),
        "json_extract_path"
        | "jsonb_extract_path"
        | "json_extract_path_text"
        | "jsonb_extract_path_text"
            if argument_count >= 2 =>
        {
            Some(true)
        }
        "json_contains" | "json_contained_by" | "json_delete_path" | "json_has_key"
        | "json_has_any_key" | "json_has_all_keys" | "jsonb_path_exists" | "jsonpath_exists"
        | "jsonb_path_match" | "jsonpath_match"
            if argument_count == 2 =>
        {
            Some(true)
        }
        "make_timestamp" if matches!(argument_count, 6 | 7) => Some(true),
        "make_interval" if argument_count <= 7 => Some(true),
        "width_bucket" if argument_count == 4 => Some(true),
        "st_dwithin" if matches!(argument_count, 2 | 3) => Some(true),
        _ => None,
    }
}

/// Return the `PostgreSQL` 18 strictness contract selected by a structural function binding. Parser-owned syntax and overload-specific built-ins must be classified by [`FunctionDispatch`], never by their diagnostic display label.
#[must_use]
pub fn bound_scalar_function_strictness(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> Option<bool> {
    let Some(binding) = binding else {
        return builtin_scalar_function_strictness(name, argument_count);
    };
    if let Some(dispatch) = binding.dispatch {
        return match dispatch {
            FunctionDispatch::ArraySubscripts
            | FunctionDispatch::Subscript
            | FunctionDispatch::BetweenSymmetric
            | FunctionDispatch::ToBinInt4
            | FunctionDispatch::ToBinInt8
            | FunctionDispatch::ToHexInt4
            | FunctionDispatch::ToHexInt8
            | FunctionDispatch::ToOctInt4
            | FunctionDispatch::ToOctInt8
            | FunctionDispatch::RandomInt4Range
            | FunctionDispatch::RandomInt8Range
            | FunctionDispatch::RandomNumericRange
            | FunctionDispatch::ArraySortJson
            | FunctionDispatch::Range { .. } => Some(true),
            FunctionDispatch::ArraySlices
            | FunctionDispatch::Slice
            | FunctionDispatch::AnyOperator
            | FunctionDispatch::AllOperator
            | FunctionDispatch::IsDistinct => Some(false),
            FunctionDispatch::NamedArgument | FunctionDispatch::VariadicArgument => None,
        };
    }
    binding
        .builtin
        .then(|| builtin_scalar_function_strictness(&binding.name, argument_count))
        .flatten()
}

/// Evaluate a call's argument list, unwrapping `name => value`
pub fn eval_bound_builtin_function_call(
    binding: &FunctionBinding,
    call_args: Vec<(Option<String>, Value)>,
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    let Some(dispatch) = binding.dispatch else {
        return eval_builtin_function_call(&binding.name, call_args, ctx);
    };
    if let Some(result) = random::eval_dispatched_random_function(dispatch, &call_args, ctx) {
        return result;
    }
    if call_args.iter().any(|(name, _)| name.is_some()) {
        return Err(SQLError::Internal(format!(
            "bound {} expression retained a named argument",
            dispatch.label()
        )));
    }
    let evaluated = call_args
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    if let Some(result) = scalar_postgres::eval_dispatched_postgres_function(dispatch, &evaluated) {
        return result;
    }
    match dispatch {
        FunctionDispatch::ArraySortJson => {
            scalar_array::eval_dispatched_json_array_sort(&evaluated)
        }
        FunctionDispatch::Range {
            operation,
            subtype,
            multirange,
        } => {
            scalar_range::eval_dispatched_range_function(operation, subtype, multirange, &evaluated)
        }
        FunctionDispatch::NamedArgument | FunctionDispatch::VariadicArgument => Err(
            SQLError::Internal("call-argument syntax marker reached scalar execution".into()),
        ),
        _ => Err(SQLError::Internal(format!(
            "{} has no scalar executor",
            dispatch.label()
        ))),
    }
}
