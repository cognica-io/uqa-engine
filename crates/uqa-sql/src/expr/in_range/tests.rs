//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::in_range;
use uqa_core::{DecimalValue, TemporalValue, Value};

fn decimal(text: &str) -> Value {
    Value::Decimal(DecimalValue::parse(text).unwrap())
}

fn interval(months: i32, days: i32, micros: i64) -> Value {
    Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    })
}

fn sqlstate(result: Result<bool, crate::SQLError>) -> String {
    result.unwrap_err().sqlstate().unwrap().to_string()
}

#[test]
fn integer_bounds_are_exact_beyond_the_ordering_type() {
    // `base + offset` beyond bigint lies above every value.
    assert!(in_range(
        &Value::Int(i64::MAX),
        &Value::Int(i64::MAX),
        &Value::Int(1),
        false,
        true
    )
    .unwrap());
    assert!(!in_range(
        &Value::Int(i64::MAX),
        &Value::Int(i64::MAX),
        &Value::Int(1),
        false,
        false
    )
    .unwrap());
    assert!(in_range(
        &Value::Int(i64::MIN),
        &Value::Int(i64::MIN),
        &Value::Int(1),
        true,
        false
    )
    .unwrap());
    assert!(in_range(&Value::Int(3), &Value::Int(5), &Value::Int(2), true, false).unwrap());
    assert!(!in_range(&Value::Int(2), &Value::Int(5), &Value::Int(2), true, false).unwrap());
    assert_eq!(
        sqlstate(in_range(
            &Value::Int(1),
            &Value::Int(1),
            &Value::Int(-1),
            true,
            false
        )),
        "22013"
    );
}

#[test]
fn float_bounds_order_nan_last_and_admit_every_value_for_opposing_infinities() {
    let nan = Value::Float(f64::NAN);
    assert!(in_range(&nan, &nan, &Value::Float(1.0), false, true).unwrap());
    assert!(in_range(&nan, &Value::Float(0.0), &Value::Float(1.0), false, false).unwrap());
    assert!(!in_range(&nan, &Value::Float(0.0), &Value::Float(1.0), false, true).unwrap());
    assert!(in_range(&Value::Float(0.0), &nan, &Value::Float(1.0), false, true).unwrap());
    let infinity = Value::Float(f64::INFINITY);
    assert!(in_range(&Value::Float(0.0), &infinity, &infinity, true, false).unwrap());
    assert!(in_range(&Value::Float(0.0), &infinity, &infinity, true, true).unwrap());
    assert_eq!(
        sqlstate(in_range(
            &Value::Float(0.0),
            &Value::Float(0.0),
            &nan,
            true,
            false
        )),
        "22013"
    );
}

#[test]
fn numeric_bounds_follow_the_special_value_rules() {
    assert!(in_range(&decimal("1.5"), &decimal("2"), &decimal("0.5"), true, false).unwrap());
    assert!(!in_range(&decimal("1.4"), &decimal("2"), &decimal("0.5"), true, false).unwrap());
    assert!(in_range(
        &decimal("-Infinity"),
        &decimal("2"),
        &decimal("Infinity"),
        true,
        true
    )
    .unwrap());
    assert!(!in_range(
        &decimal("5"),
        &decimal("2"),
        &decimal("Infinity"),
        true,
        true
    )
    .unwrap());
    assert!(in_range(
        &decimal("5"),
        &decimal("Infinity"),
        &decimal("Infinity"),
        true,
        true
    )
    .unwrap());
    assert!(in_range(
        &decimal("Infinity"),
        &decimal("1"),
        &decimal("1"),
        false,
        false
    )
    .unwrap());
    assert!(in_range(&decimal("NaN"), &decimal("1"), &decimal("1"), false, false).unwrap());
    for offset in ["-1", "NaN", "-Infinity"] {
        assert_eq!(
            sqlstate(in_range(
                &decimal("1"),
                &decimal("1"),
                &decimal(offset),
                false,
                false
            )),
            "22013"
        );
    }
}

#[test]
fn time_bounds_ignore_months_and_days_and_do_not_wrap() {
    let time = |micros| Value::Temporal(TemporalValue::Time { micros });
    let hour = 3_600_000_000;
    // One hour after 23:30 lies past midnight rather than wrapping to 00:30.
    assert!(!in_range(
        &time(hour / 2),
        &time(23 * hour + hour / 2),
        &interval(1, 1, hour),
        false,
        false
    )
    .unwrap());
    assert!(in_range(
        &time(22 * hour),
        &time(23 * hour),
        &interval(0, 0, hour),
        true,
        false
    )
    .unwrap());
    assert_eq!(
        sqlstate(in_range(
            &time(0),
            &time(0),
            &interval(1, 0, -1),
            true,
            false
        )),
        "22013"
    );
}

#[test]
fn date_and_interval_bounds_use_calendar_arithmetic_and_interval_order() {
    let date = |days| Value::Temporal(TemporalValue::Date { days });
    // 2024-01-31 plus one month is 2024-02-29.
    let january_31 = 19_753;
    let february_29 = 19_782;
    assert!(in_range(
        &date(february_29),
        &date(january_31),
        &interval(1, 0, 0),
        false,
        true
    )
    .unwrap());
    assert!(!in_range(
        &date(february_29 + 1),
        &date(january_31),
        &interval(1, 0, 0),
        false,
        true
    )
    .unwrap());
    // `1 mon -40 days` is negative: 30 - 40 days.
    assert_eq!(
        sqlstate(in_range(
            &date(0),
            &date(0),
            &interval(1, -40, 0),
            true,
            false
        )),
        "22013"
    );
    assert!(in_range(
        &interval(0, 30, 0),
        &interval(0, 0, 0),
        &interval(1, 0, 0),
        false,
        true
    )
    .unwrap());
    assert!(!in_range(
        &interval(0, 31, 0),
        &interval(0, 0, 0),
        &interval(1, 0, 0),
        false,
        true
    )
    .unwrap());
}
