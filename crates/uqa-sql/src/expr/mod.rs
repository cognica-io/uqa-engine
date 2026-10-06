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
mod floating;
mod in_range;
pub(crate) mod integer_input;
mod json;
mod json_strip;
mod random;
mod range;
mod regex;
mod time;
mod uuid;

pub use array_transform::{
    argument_positions as array_transform_argument_positions,
    argument_positions_with_control as array_transform_argument_positions_with_control,
};
pub use json::{validate_json_object_key_type, value_to_json_text};
pub use json_strip::argument_positions as json_strip_nulls_argument_positions;
pub use range::{
    multirange_from_ranges, parse_multirange, parse_range, CanonicalMultirange, CanonicalRange,
};
pub use time::IntervalFields;
use time::{
    age_between, coerce_temporal, format_pg_number, format_temporal, hex_encode, make_timestamp,
    parse_timestamp, pg_to_chrono_fmt,
};
pub use uuid::parse_uuid_bytes;
use uuid::{generate_random_uuid, generate_uuid_v7};
mod binary;
mod casting;
pub mod composites;
mod conversion;
mod current_time;
pub mod enums;
pub(crate) mod hashing;
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
mod session_settings;
mod session_sleep;
pub mod variadic_any;

#[cfg(test)]
use binary::eval_comparison_op;
pub use binary::{
    compare_nullable_with_control, compare_typed_values_with_control, compare_with_control,
    eval_binary_values, eval_binary_values_with_control, eval_binary_values_with_integer_width,
    eval_binary_values_with_integer_width_with_control, eval_comparison_truth,
    eval_comparison_truth_with_control, integer_width_for_literal, integer_width_for_type, truthy,
    type_comparison_can_fail, validate_legacy_vector_comparison, value_comparison_can_fail,
    values_equal_nullable_with_control, values_equal_with_control, IntegerWidth,
};
pub(crate) use binary::{datetime_out_of_range, division_by_zero, out_of_range};
pub use casting::{
    array_dimensions, cast_value, cast_value_from, cast_value_from_with_control,
    invalid_boolean_input, negate_value, negate_value_with_control, parse_boolean_input,
    parse_pg_array_literal, parse_pg_array_literal_with_control,
};
use conversion::{
    allocation_error, float_to_i64_rounded, float_to_i64_trunc, nonnegative_usize, to_decimal,
    to_i64,
};
pub use conversion::{
    array_value_to_string, catalog_output_required, invalid_numeric_input, value_to_string,
    value_to_string_with_control, vector_value_to_string,
};
pub(crate) use conversion::{tensor_items, vector_element, vector_items};
pub use conversion::{
    value_to_tensor, value_to_tensor_with_control, value_to_vector, value_to_vector_with_control,
};
pub(crate) use current_time::transaction_timestamp_or_clock;
pub use current_time::{clock_timestamp_micros, transaction_clock_micros, TransactionClockScope};
pub use floating::{
    eval_float_arithmetic, eval_float_arithmetic_with_control, format_real, FloatWidth,
};
pub use in_range::in_range;
#[cfg(test)]
use scalar_dispatch::eval_scalar_function;
use scalar_helpers::{point_xy, typeof_value};
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
    validate_named_argument_order_with_control, variadic_argument_value, wrap_variadic_argument,
};
pub use call_dispatch::{
    eval_builtin_function_call, eval_function_call, eval_generated_function_call_with_control,
};
pub(crate) use context::requires_domain_array_input;
pub use context::{
    cast_value_with_type_resolution, cast_value_with_type_resolution_with_control,
    coercion_type_name, format_regtype_value, format_regtype_value_with_control,
    read_catalog_array_input, CatalogInputFunctions, EngineHook, EvalContext, RowLookup,
};
pub use diagnostics::{unknown_function_error, value_type_name};
pub use evaluator::eval;
mod numeric_operator;
use evaluator::eval_between_with_control;
pub use numeric_operator::{eval_numeric_operator, eval_numeric_operator_with_control};

#[cfg(test)]
mod tests;

mod json_carrier;
pub use json_carrier::{core_value_to_json, value_to_text, value_to_text_with_control};
