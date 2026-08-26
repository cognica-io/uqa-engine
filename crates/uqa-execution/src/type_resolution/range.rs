//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind range-polymorphic functions while declared range identity is available.

use uqa_sql::ast::{
    ColumnType, FunctionBinding, FunctionDispatch, RangeFunctionOperation, RangeSubtype,
};
use uqa_sql::SQLParam;

use crate::{RowSchema, ScalarExpr};

use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() {
        return name;
    }
    let local = name
        .strip_prefix("pg_catalog.")
        .unwrap_or(&name)
        .to_ascii_lowercase();
    if !matches!(
        local.as_str(),
        "lower"
            | "upper"
            | "isempty"
            | "lower_inc"
            | "upper_inc"
            | "lower_inf"
            | "upper_inf"
            | "range_merge"
            | "multirange"
            | "array_overlap"
            | "contains_op"
            | "contained_by_op"
            | "range_adjacent"
    ) {
        return name;
    }
    let Some(first) = args.first() else {
        return name;
    };
    let Ok(Some(first_type)) = scalar_type_inner(first, schema, params, resolver) else {
        return name;
    };
    let Some((subtype, _type_name, multirange)) = range_identity(&first_type) else {
        return name;
    };
    let operation = match local.as_str() {
        "lower" => RangeFunctionOperation::Lower,
        "upper" => RangeFunctionOperation::Upper,
        "isempty" => RangeFunctionOperation::IsEmpty,
        "lower_inc" => RangeFunctionOperation::LowerInclusive,
        "upper_inc" => RangeFunctionOperation::UpperInclusive,
        "lower_inf" => RangeFunctionOperation::LowerInfinite,
        "upper_inf" => RangeFunctionOperation::UpperInfinite,
        "range_merge" => RangeFunctionOperation::Merge,
        "multirange" => RangeFunctionOperation::Multirange,
        "array_overlap" => RangeFunctionOperation::Overlap,
        "contains_op" => RangeFunctionOperation::Contains,
        "contained_by_op" => RangeFunctionOperation::ContainedBy,
        "range_adjacent" => RangeFunctionOperation::Adjacent,
        _ => unreachable!("range function name was checked above"),
    };
    *binding = Some(FunctionBinding::dispatched(FunctionDispatch::Range {
        operation,
        subtype,
        multirange,
    }));
    name
}

pub(super) fn function_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_types: &[Option<ColumnType>],
) -> Option<ColumnType> {
    if let Some(FunctionDispatch::Range {
        operation, subtype, ..
    }) = binding.and_then(|binding| binding.dispatch)
    {
        return Some(match operation {
            RangeFunctionOperation::Lower | RangeFunctionOperation::Upper => subtype.scalar_type(),
            RangeFunctionOperation::Merge => ColumnType::Range(subtype),
            RangeFunctionOperation::Multirange => ColumnType::Multirange(subtype),
            RangeFunctionOperation::IsEmpty
            | RangeFunctionOperation::LowerInclusive
            | RangeFunctionOperation::UpperInclusive
            | RangeFunctionOperation::LowerInfinite
            | RangeFunctionOperation::UpperInfinite
            | RangeFunctionOperation::Overlap
            | RangeFunctionOperation::Contains
            | RangeFunctionOperation::ContainedBy
            | RangeFunctionOperation::Adjacent => ColumnType::Boolean,
        });
    }
    let local = name
        .strip_prefix("pg_catalog.")
        .unwrap_or(name)
        .to_ascii_lowercase();
    if let Some(subtype) = subtype_for_constructor(&local) {
        return Some(if local == subtype.range_name() {
            ColumnType::Range(subtype)
        } else {
            ColumnType::Multirange(subtype)
        });
    }
    let first = argument_types.first()?.as_ref()?;
    let (subtype, _, _) = range_identity(first)?;
    match local.as_str() {
        "lower" | "upper" => Some(subtype.scalar_type()),
        "isempty" | "lower_inc" | "upper_inc" | "lower_inf" | "upper_inf" | "array_overlap"
        | "contains_op" | "contained_by_op" | "range_adjacent" => Some(ColumnType::Boolean),
        "range_merge" => Some(ColumnType::Range(subtype)),
        "multirange" => Some(ColumnType::Multirange(subtype)),
        _ => None,
    }
}

fn range_identity(ty: &ColumnType) -> Option<(RangeSubtype, &'static str, bool)> {
    match ty {
        ColumnType::Range(subtype) => Some((*subtype, subtype.range_name(), false)),
        ColumnType::Multirange(subtype) => Some((*subtype, subtype.multirange_name(), true)),
        ColumnType::Domain { base, .. } => range_identity(base),
        _ => None,
    }
}

fn subtype_for_constructor(name: &str) -> Option<RangeSubtype> {
    [
        RangeSubtype::Integer,
        RangeSubtype::BigInteger,
        RangeSubtype::Numeric,
        RangeSubtype::Date,
        RangeSubtype::Timestamp,
        RangeSubtype::TimestampTz,
    ]
    .into_iter()
    .find(|subtype| name == subtype.range_name() || name == subtype.multirange_name())
}
