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

mod array_access;
pub use array_access::eval_array_access_with_control;
mod array_transform;
pub mod bit_string;
pub mod datums;
mod encoding;
mod floating;
mod in_range;
pub(crate) mod integer_input;
mod json;
mod json_strip;
mod random;
mod range;
pub(crate) use range::datum as range_datum;
mod regex;
pub use regex::{check_constant_similar_translation, RegexMatches, RegexSplit};
pub use regex::{current_regex_cache, RegexCache, RegexCacheScope};
mod time;
mod uuid;

pub use array_transform::{
    argument_positions as array_transform_argument_positions,
    argument_positions_with_control as array_transform_argument_positions_with_control,
};
pub(crate) use json::{decode_jsonb_datum_with_control, encode_jsonb_datum};
pub use json::{validate_json_object_key_type, value_to_json_text};
pub use json_strip::argument_positions as json_strip_nulls_argument_positions;
pub use range::{
    declared_multirange_datum_with_control, declared_range_datum_with_control,
    multirange_from_ranges, multirange_from_ranges_with_operations, parse_multirange,
    parse_multirange_with_operations, parse_range, parse_range_with_operations, range_bound_flags,
    range_from_bounds_with_operations, CanonicalMultirange, CanonicalRange, RangeComparisonValue,
    RangeConstruction, RangeValueDescriptor,
};
pub use time::IntervalFields;
use time::{
    age_between, coerce_temporal, format_pg_number, format_temporal, hex_encode, make_timestamp,
    parse_timestamp, pg_to_chrono_fmt,
};
pub(crate) use uuid::format_uuid;
use uuid::generate_random_uuid;
pub use uuid::parse_uuid_bytes;
mod binary;
mod casting;
mod collation;
pub use collation::compare_unicode_with_control;
mod collation_for;
pub mod composites;
mod concatenation;
mod conversion;
mod current_time;
pub mod enums;
pub(crate) mod hashing;
pub(crate) mod internal_char;
pub(crate) mod interval_style;
mod pattern;
mod scalar_array;
mod scalar_core;
pub(crate) use scalar_core::binds_input_collation_by_name;
mod scalar_dispatch;
mod scalar_geospatial;
mod scalar_helpers;
mod scalar_json;
mod scalar_math;
mod scalar_postgres;
pub use scalar_postgres::eval_distinct_boolean;
mod scalar_range;
mod scalar_temporal;
mod session_settings;
mod session_sleep;
mod statement_cancellation;
mod temporal_input;
mod value_catalog;
pub mod variadic_any;
pub use value_catalog::{EngineValueCatalog, SQLRecordFields, SQLValueCatalog};

#[cfg(test)]
use binary::eval_comparison_op;
pub use binary::{
    compare_btree_inputs_with_catalog_and_control, compare_btree_inputs_with_invocation_state,
    compare_grouping_values_with_enum_catalog, compare_nullable_with_control,
    compare_record_images_with_catalog, compare_selected_grouping_values_with_catalog,
    compare_selected_values_with_catalog, compare_typed_values_with_catalog_and_collation,
    compare_typed_values_with_control, compare_typed_values_with_enum_catalog,
    compare_with_control, equal_selected_values_with_catalog, equal_selected_values_with_engine,
    equal_typed_values_with_catalog_and_collation, equal_typed_values_with_enum_catalog,
    eval_binary_values, eval_binary_values_with_control, eval_binary_values_with_engine,
    eval_binary_values_with_integer_width, eval_binary_values_with_integer_width_with_control,
    eval_comparison_truth, eval_comparison_truth_with_control,
    eval_comparison_truth_with_deferred_state, eval_comparison_truth_with_engine,
    eval_comparison_truth_with_enum_catalog, eval_record_comparison_truth_with_engine,
    eval_value_comparison_truth_with_engine, integer_width_for_literal, integer_width_for_type,
    truthy, type_comparison_can_fail, validate_legacy_vector_comparison, value_comparison_can_fail,
    values_equal_nullable_with_control, values_equal_with_control,
    visit_record_image_fields_with_catalog, with_btree_input, BTreeInput, IntegerWidth,
};
pub(crate) use binary::{datetime_out_of_range, division_by_zero, out_of_range};
pub(crate) use casting::text_input_is_session_independent;
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
pub(crate) use conversion::{
    bytea_to_string_with_control, tensor_items, vector_element, vector_items,
    vector_value_to_string_with_control,
};
pub use conversion::{
    value_to_tensor, value_to_tensor_with_control, value_to_vector, value_to_vector_with_control,
};
pub(crate) use current_time::transaction_timestamp_or_clock;
pub use current_time::{clock_timestamp_micros, transaction_clock_micros, TransactionClockScope};
pub(crate) use floating::format_real_with_control;
pub use floating::{
    eval_float_arithmetic, eval_float_arithmetic_with_control, format_real, FloatWidth,
};
pub use in_range::{in_range, in_range_with_engine};
#[cfg(test)]
use scalar_dispatch::eval_scalar_function;
pub(crate) use scalar_helpers::{append_sql_literal, quote_ident_with_control};
use scalar_helpers::{point_xy, typeof_value};
pub use scalar_helpers::{quote_ident, CompiledLikePattern};
pub use statement_cancellation::{
    check_statement_cancellation, statement_cancellation, StatementCancellationScope,
};
pub use temporal_input::{
    temporal_date_order, temporal_interval_style, DateOrderScope, IntervalStyleScope,
    TemporalInputContext, TemporalInputScope,
};

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
    call_argument_name, call_argument_value, evaluate_call_args, validate_named_argument_order,
    validate_named_argument_order_with_control, variadic_argument_value, wrap_variadic_argument,
};
pub use call_dispatch::{
    builtin_named_args, eval_builtin_function_call, eval_builtin_function_call_with_control,
    eval_function_call, eval_generated_function_call_with_control,
};
pub use context::{
    cast_value_with_type_resolution, cast_value_with_type_resolution_with_control,
    coercion_type_name, format_regtype_value, format_regtype_value_with_control,
    read_catalog_array_input, read_catalog_input, CatalogInputFunctions, EngineHook, EvalContext,
    RowKeySpace, RowLookup,
};
pub(crate) use context::{
    format_regtype_value_with_catalog_and_control, requires_catalog_constant_input,
    requires_domain_array_input,
};
pub use diagnostics::{unknown_function_error, value_type_name};
pub use evaluator::eval;
mod numeric_operator;
pub use numeric_operator::{eval_numeric_operator, eval_numeric_operator_with_control};

#[cfg(test)]
mod tests;

mod json_carrier;
pub use json_carrier::{core_value_to_json, value_to_text, value_to_text_with_control};
