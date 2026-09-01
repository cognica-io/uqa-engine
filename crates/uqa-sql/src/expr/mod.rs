//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};

use crate::ast::{BinaryOp, Expr};
#[cfg(test)]
use crate::ast::{ColumnType, FunctionBinding, FunctionDispatch};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;
#[cfg(test)]
use crate::result::ResultRow;

mod array_transform;
mod encoding;
mod json;
mod json_strip;
mod random;
mod range;
mod time;
mod uuid;

pub use array_transform::argument_positions as array_transform_argument_positions;
use encoding::{base64_decode, base64_encode, md5_hex};
pub use json::value_to_json_text;
use json::{
    format_jsonb_pretty, json_build_array_value, json_build_object_value, json_concat,
    json_contained_by, json_contains, json_delete, json_delete_path, json_extract_path,
    json_has_key, json_has_keys, json_typeof, jsonb_insert, jsonb_set, jsonpath_exists,
    jsonpath_match, parse_json, strip_nulls, typed_json_value, value_to_json,
};
pub use json_strip::argument_positions as json_strip_nulls_argument_positions;
use json_strip::strip_json_nulls_text;
pub use range::{
    multirange_from_ranges, parse_multirange, parse_range, CanonicalMultirange, CanonicalRange,
};
use time::{
    age_between, coerce_temporal, date_trunc_value, extract_from_value, format_pg_number,
    format_temporal, hex_encode, make_timestamp, parse_timestamp, pg_to_chrono_fmt,
};
pub use uuid::parse_uuid_bytes;
use uuid::{extract_uuid_timestamp, extract_uuid_version, generate_random_uuid, generate_uuid_v7};
mod binary;
mod casting;
mod conversion;
mod scalar_array;
mod scalar_core;
mod scalar_dispatch;
mod scalar_geospatial;
mod scalar_helpers;
mod scalar_json;
mod scalar_math;
mod scalar_postgres;
mod scalar_range;
mod scalar_temporal;

use binary::{compare, eval_comparison_op, values_equal};
pub(crate) use binary::{division_by_zero, out_of_range};
pub use binary::{
    eval_binary_values, eval_binary_values_with_integer_width, eval_comparison_truth,
    integer_width_for_literal, integer_width_for_type, truthy, IntegerWidth,
};
pub use casting::{
    array_dimensions, cast_value, cast_value_from, negate_value, parse_pg_array_literal,
};
pub(crate) use conversion::to_f64;
use conversion::{
    allocation_error, coerce_i64, expect_str, float1, float_to_i64_rounded, float_to_i64_trunc,
    gcd_i64, initcap_str, nonnegative_usize, string1, to_decimal, to_i64,
};
pub use conversion::{array_value_to_string, value_to_string, vector_value_to_string};
pub use conversion::{value_to_tensor, value_to_vector};
#[cfg(test)]
use scalar_dispatch::eval_scalar_function;
use scalar_helpers::{
    compile_pg_regex, point_xy, quote_literal, similar_to_regex, trim_chars, typeof_value,
};
pub use scalar_helpers::{quote_ident, CompiledLikePattern};

mod builtin;
mod call_arguments;
mod call_dispatch;
mod context;
mod diagnostics;
mod evaluator;

pub use builtin::{
    bound_scalar_function_strictness, builtin_scalar_function_strictness,
    eval_bound_builtin_function_call,
};
pub use call_arguments::{
    call_argument_value, evaluate_call_args, validate_named_argument_order,
    variadic_argument_value, wrap_variadic_argument,
};
pub use call_dispatch::{eval_builtin_function_call, eval_function_call};
pub use context::{
    cast_value_with_type_resolution, coercion_type_name, format_regtype_value, EngineHook,
    EvalContext, RowLookup,
};
pub use diagnostics::{unknown_function_error, value_type_name};
pub use evaluator::eval;
use evaluator::eval_between;

#[cfg(test)]
mod tests;
