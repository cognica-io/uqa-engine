//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical range values used by temporal keys and references.
use crate::{
    ast::RangeSubtype,
    expr::{parse_multirange, parse_range, CanonicalRange},
    ColumnType, SQLError,
};
use uqa_core::Value;

pub fn period_ranges(
    value: &Value,
    column_type: &ColumnType,
) -> Result<(RangeSubtype, Vec<CanonicalRange>), SQLError> {
    let (Value::Str(text) | Value::FixedChar(text)) = value else {
        return Err(SQLError::TypeMismatch(format!(
            "PERIOD value has incompatible runtime carrier {value:?}"
        )));
    };
    match column_type {
        ColumnType::Range(subtype) => {
            let range = parse_range(text, *subtype)?;
            Ok((
                *subtype,
                (!range.is_empty()).then_some(range).into_iter().collect(),
            ))
        }
        ColumnType::Multirange(subtype) => Ok((
            *subtype,
            parse_multirange(text, *subtype)?.ranges().to_vec(),
        )),
        other => Err(SQLError::TypeMismatch(format!(
            "PERIOD column must be a range or multirange, got {}",
            other.sql_name()
        ))),
    }
}

pub fn period_values_overlap(
    left: &Value,
    right: &Value,
    column_type: &ColumnType,
) -> Result<bool, SQLError> {
    let (left_subtype, left) = period_ranges(left, column_type)?;
    let (right_subtype, right) = period_ranges(right, column_type)?;
    Ok(left_subtype == right_subtype
        && left
            .iter()
            .any(|left| right.iter().any(|right| left.overlaps(right))))
}
