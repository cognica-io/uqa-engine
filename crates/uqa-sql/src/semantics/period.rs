//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical range values used by temporal keys and references.
use crate::{
    ast::RangeSubtype,
    expr::{CanonicalRange, RangeComparisonValue},
    ColumnType, SQLError,
};
use uqa_core::Value;

pub fn period_ranges(
    value: &Value,
    column_type: &ColumnType,
) -> Result<(RangeSubtype, Vec<CanonicalRange>), SQLError> {
    let subtype = match column_type {
        ColumnType::Range(subtype) | ColumnType::Multirange(subtype) => *subtype,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "PERIOD column must be a range or multirange, got {}",
                other.sql_name()
            )))
        }
    };
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let ranges =
        match RangeComparisonValue::from_type_with_control(value, column_type, None, &control)? {
            Some(RangeComparisonValue::Range(range)) => (!range.is_empty())
                .then(|| range.into_uncontrolled().expect("PERIOD range"))
                .into_iter()
                .collect(),
            Some(RangeComparisonValue::Multirange(ranges)) => ranges.ranges().to_vec(),
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "PERIOD value has incompatible runtime carrier {value:?}"
                )))
            }
        };
    Ok((subtype, ranges))
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
