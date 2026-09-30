//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `transformFrameOffset` for `RANGE` frames: the type an offset takes, chosen among the `in_range` support functions of the btree operator family that orders the frame's `ORDER BY` column.

use crate::ast::ColumnType;
use crate::SQLError;

use super::common::base_type;

/// The offset types of the `in_range` support functions registered for an ordering type in the built-in btree operator families.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OffsetType {
    SmallInteger,
    Integer,
    BigInteger,
    DoublePrecision,
    Numeric,
    Interval,
}

impl OffsetType {
    fn column_type(self) -> ColumnType {
        match self {
            Self::SmallInteger => ColumnType::SmallInteger,
            Self::Integer => ColumnType::Integer,
            Self::BigInteger => ColumnType::BigInteger,
            Self::DoublePrecision => ColumnType::DoublePrecision,
            Self::Numeric => ColumnType::Numeric {
                precision: None,
                scale: None,
            },
            Self::Interval => ColumnType::Interval,
        }
    }

    const fn canonical_name(self) -> &'static str {
        match self {
            Self::SmallInteger => "int2",
            Self::Integer => "int4",
            Self::BigInteger => "int8",
            Self::DoublePrecision => "float8",
            Self::Numeric => "numeric",
            Self::Interval => "interval",
        }
    }
}

/// The `in_range` offset types for the input type of the operator class that orders `ty`, as `pg_amproc` lists them.
fn in_range_offset_types(ty: &ColumnType) -> &'static [OffsetType] {
    match ty {
        ColumnType::SmallInteger | ColumnType::Integer => &[
            OffsetType::BigInteger,
            OffsetType::SmallInteger,
            OffsetType::Integer,
        ],
        ColumnType::BigInteger => &[OffsetType::BigInteger],
        ColumnType::Real | ColumnType::DoublePrecision => &[OffsetType::DoublePrecision],
        ColumnType::Numeric { .. } => &[OffsetType::Numeric],
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz
        | ColumnType::Interval => &[OffsetType::Interval],
        _ => &[],
    }
}

/// The input type of the btree operator class that orders values of `ty`, as `format_type_be` names it: a domain is ordered by its base type's class, and the polymorphic classes take their pseudo-types.
fn ordering_class_input_name(ty: &ColumnType) -> String {
    match base_type(ty) {
        ColumnType::Varchar(_) => "text".into(),
        ColumnType::Bpchar | ColumnType::Character(_) => "character".into(),
        ColumnType::Enum(_) => "anyenum".into(),
        ColumnType::Array(_) => "anyarray".into(),
        ColumnType::Range(_) => "anyrange".into(),
        ColumnType::Multirange(_) => "anymultirange".into(),
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype => "oid".into(),
        other => other.regtype_name(),
    }
}

/// Whether a value of type `actual` coerces implicitly to `target`, as `can_coerce_type` decides it: a domain first becomes its base type.
fn coerces_implicitly(actual: &ColumnType, target: OffsetType) -> bool {
    let actual = super::canonical_column_type_name(base_type(actual));
    actual == target.canonical_name()
        || super::routine_type_accepts_implicit_cast(&actual, target.canonical_name())
}

/// The type a `RANGE` frame offset of type `offset` (`None` for an `unknown` literal or parameter) is coerced to when the frame is ordered by a column of type `order` (`None` when the key is itself `unknown`, which sorts as `text`). An offset of the exact type of one support function selects it; an `unknown` offset prefers the ordering type; any other offset must coerce implicitly to exactly one support function's type.
pub fn range_frame_offset_type(
    order: Option<&ColumnType>,
    offset: Option<&ColumnType>,
) -> Result<ColumnType, SQLError> {
    let ordering = order.map_or(ColumnType::Text, |order| base_type(order).clone());
    let candidates = in_range_offset_types(&ordering);
    let column_name = || order.map_or_else(|| "text".into(), ordering_class_input_name);
    if candidates.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "RANGE with offset PRECEDING/FOLLOWING is not supported for column type {}",
                column_name()
            ),
        });
    }
    let preferred = match offset {
        Some(offset) => super::canonical_column_type_name(offset),
        None => super::canonical_column_type_name(&ordering),
    };
    let matches = candidates
        .iter()
        .copied()
        .filter(|candidate| offset.is_none_or(|offset| coerces_implicitly(offset, *candidate)))
        .collect::<Vec<_>>();
    let offset_name = || offset.map_or_else(|| "unknown".into(), ColumnType::regtype_name);
    if matches.is_empty() {
        return Err(SQLError::Diagnostic {
            sqlstate: "42883".into(),
            message: format!(
                "RANGE with offset PRECEDING/FOLLOWING is not supported for column type {} and offset type {}",
                column_name(),
                offset_name()
            ),
            detail: None,
            hint: Some("Cast the offset value to an appropriate type.".into()),
        });
    }
    if let Some(selected) = matches
        .iter()
        .find(|candidate| candidate.canonical_name() == preferred)
    {
        return Ok(selected.column_type());
    }
    if let [selected] = matches.as_slice() {
        return Ok(selected.column_type());
    }
    Err(SQLError::Diagnostic {
        sqlstate: "42725".into(),
        message: format!(
            "RANGE with offset PRECEDING/FOLLOWING has multiple interpretations for column type {} and offset type {}",
            column_name(),
            offset_name()
        ),
        detail: None,
        hint: Some("Cast the offset value to the exact intended type.".into()),
    })
}

#[cfg(test)]
mod tests;
