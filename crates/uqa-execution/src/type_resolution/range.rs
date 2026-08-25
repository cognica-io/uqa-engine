//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind range-polymorphic functions while declared range identity is available.

use uqa_sql::ast::{ColumnType, RangeSubtype};
use uqa_sql::SQLParam;

use crate::{RowSchema, ScalarExpr};

use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn bind_call(
    name: String,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
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
    let Some((_subtype, type_name)) = range_identity(&first_type) else {
        return name;
    };
    let operation = match local.as_str() {
        "range_merge" => "merge",
        "array_overlap" => "overlap",
        "contains_op" => "contains",
        "contained_by_op" => "contained_by",
        "range_adjacent" => "adjacent",
        operation => operation,
    };
    format!("__range_{operation}_{type_name}")
}

pub(super) fn function_type(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Option<ColumnType> {
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
    let (subtype, _) = range_identity(first)?;
    match local.as_str() {
        "lower" | "upper" => Some(subtype.scalar_type()),
        "isempty" | "lower_inc" | "upper_inc" | "lower_inf" | "upper_inf" | "array_overlap"
        | "contains_op" | "contained_by_op" | "range_adjacent" => Some(ColumnType::Boolean),
        "range_merge" => Some(ColumnType::Range(subtype)),
        "multirange" => Some(ColumnType::Multirange(subtype)),
        _ if local.starts_with("__range_") => {
            if local.contains("_lower_") || local.contains("_upper_") {
                Some(subtype.scalar_type())
            } else if local.contains("_merge_") {
                Some(ColumnType::Range(subtype))
            } else if local.contains("_multirange_") {
                Some(ColumnType::Multirange(subtype))
            } else {
                Some(ColumnType::Boolean)
            }
        }
        _ => None,
    }
}

fn range_identity(ty: &ColumnType) -> Option<(RangeSubtype, &'static str)> {
    match ty {
        ColumnType::Range(subtype) => Some((*subtype, subtype.range_name())),
        ColumnType::Multirange(subtype) => Some((*subtype, subtype.multirange_name())),
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
