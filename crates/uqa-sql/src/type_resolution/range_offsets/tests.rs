//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::range_frame_offset_type;
use crate::ast::ColumnType;

fn numeric() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "d".into(),
        oid: 16_384,
        array_oid: None,
        base: Box::new(base),
    }
}

fn error(
    order: Option<&ColumnType>,
    offset: Option<&ColumnType>,
) -> (String, String, Option<String>) {
    let error = range_frame_offset_type(order, offset).unwrap_err();
    (
        error.sqlstate().unwrap().to_string(),
        error.to_string(),
        error.hint().map(str::to_string),
    )
}

#[test]
fn offsets_select_the_exact_or_preferred_support_function() {
    let integer = ColumnType::Integer;
    assert_eq!(
        range_frame_offset_type(Some(&integer), Some(&ColumnType::Integer)).unwrap(),
        ColumnType::Integer
    );
    assert_eq!(
        range_frame_offset_type(Some(&integer), Some(&ColumnType::BigInteger)).unwrap(),
        ColumnType::BigInteger
    );
    assert_eq!(
        range_frame_offset_type(Some(&ColumnType::SmallInteger), None).unwrap(),
        ColumnType::SmallInteger
    );
    assert_eq!(
        range_frame_offset_type(Some(&numeric()), Some(&ColumnType::Integer)).unwrap(),
        numeric()
    );
    assert_eq!(
        range_frame_offset_type(Some(&ColumnType::Real), None).unwrap(),
        ColumnType::DoublePrecision
    );
    assert_eq!(
        range_frame_offset_type(Some(&ColumnType::Date), None).unwrap(),
        ColumnType::Interval
    );
    assert_eq!(
        range_frame_offset_type(Some(&domain(ColumnType::TimestampTz)), None).unwrap(),
        ColumnType::Interval
    );
}

#[test]
fn unsupported_orderings_and_offsets_report_postgresql_errors() {
    assert_eq!(
        error(Some(&ColumnType::Text), Some(&ColumnType::Integer)),
        (
            "0A000".into(),
            "RANGE with offset PRECEDING/FOLLOWING is not supported for column type text".into(),
            None
        )
    );
    assert_eq!(
        error(Some(&ColumnType::Varchar(Some(3))), None).1,
        "RANGE with offset PRECEDING/FOLLOWING is not supported for column type text"
    );
    assert_eq!(
        error(Some(&ColumnType::Integer), Some(&numeric())),
        (
            "42883".into(),
            "RANGE with offset PRECEDING/FOLLOWING is not supported for column type integer and offset type numeric".into(),
            Some("Cast the offset value to an appropriate type.".into())
        )
    );
    // A domain over smallint coerces to every integer offset type but is none of them.
    let (sqlstate, message, hint) = error(
        Some(&ColumnType::Integer),
        Some(&domain(ColumnType::SmallInteger)),
    );
    assert_eq!(sqlstate, "42725");
    assert_eq!(
        message,
        "RANGE with offset PRECEDING/FOLLOWING has multiple interpretations for column type integer and offset type d"
    );
    assert_eq!(
        hint.as_deref(),
        Some("Cast the offset value to the exact intended type.")
    );
}
