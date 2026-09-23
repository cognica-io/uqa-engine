//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue};

fn evaluate(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Produced<Value> {
    eval_temporal_functions_with_control(name, args, control)
        .unwrap()
        .unwrap()
}

#[test]
fn temporal_results_preserve_decimal_scale_and_release_only_their_workspace() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let interval = Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: 0,
        micros: 1_234_567,
    });
    let extracted = evaluate(
        "extract",
        &[Value::Str("SECOND".into()), interval.clone()],
        &control,
    );
    let Value::Decimal(value) = &*extracted else {
        panic!("numeric extraction");
    };
    assert_eq!(value.to_sql_string(), "1.234567");
    assert!(extracted.reserved_bytes() > 0);
    assert_eq!(budget.used(), extracted.reserved_bytes());
    let retained = budget.used();
    for (name, args, expected) in construction_cases(interval)
        .into_iter()
        .chain(transform_cases())
    {
        let output = evaluate(name, &args, &control);
        assert_eq!(*output, expected, "{name}");
        assert_eq!(output.reserved_bytes(), 0, "{name}");
        assert_eq!(budget.used(), retained, "{name}");
    }
    drop(extracted);
    assert_eq!(budget.used(), 0);
}

#[test]
fn temporal_quota_and_both_cancellations_preserve_resource_errors() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = Value::Temporal(TemporalValue::Time { micros: 1_250_000 });
    let error = super::super::super::time::extract_from_value("second", &value, true, &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert_eq!(
        *super::super::super::time::extract_from_value("second", &value, false, &control).unwrap(),
        Value::Float(1.25)
    );
    assert_eq!(
        *evaluate("make_interval", &[], &control),
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 0,
            micros: 0
        })
    );
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(4096);
        let control = ProductionControl::new(&budget, &original, &invoking);
        for name in ["extract", "make_date", "age", "uuid_extract_version"] {
            let error = eval_temporal_functions_with_control(name, &[], &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
    for name in [
        "clock_timestamp",
        "to_char",
        "to_date",
        "to_number",
        "uuidv7",
    ] {
        assert!(eval_temporal_functions_with_control(name, &[], &control).is_none());
    }
}

fn construction_cases(interval: Value) -> [(&'static str, Vec<Value>, Value); 5] {
    [
        (
            "date_part",
            vec![Value::Str("second".into()), interval],
            Value::Float(1.234_567),
        ),
        (
            "to_timestamp",
            vec![Value::Decimal(DecimalValue::parse("1.25").unwrap())],
            Value::Temporal(TemporalValue::TimestampTz { micros: 1_250_000 }),
        ),
        (
            "make_timestamp",
            vec![
                Value::Int(1970),
                Value::Int(1),
                Value::Int(1),
                Value::Int(0),
                Value::Int(0),
                Value::Float(1.25),
            ],
            Value::Temporal(TemporalValue::Timestamp { micros: 1_250_000 }),
        ),
        (
            "make_date",
            vec![Value::Int(1970), Value::Int(1), Value::Int(2)],
            Value::Temporal(TemporalValue::Date { days: 1 }),
        ),
        (
            "make_interval",
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4),
                Value::Int(5),
                Value::Int(6),
                Value::Float(7.25),
            ],
            Value::Temporal(TemporalValue::Interval {
                months: 14,
                days: 25,
                micros: 18_367_250_000,
            }),
        ),
    ]
}

fn transform_cases() -> [(&'static str, Vec<Value>, Value); 6] {
    [
        (
            "date_trunc",
            vec![
                Value::Str("DAY".into()),
                Value::Temporal(TemporalValue::Timestamp {
                    micros: 86_401_234_567,
                }),
            ],
            Value::Temporal(TemporalValue::Timestamp {
                micros: 86_400_000_000,
            }),
        ),
        (
            "age",
            vec![
                Value::Temporal(TemporalValue::Date { days: 2 }),
                Value::Temporal(TemporalValue::Date { days: 1 }),
            ],
            Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: 1,
                micros: 0,
            }),
        ),
        (
            "justify_hours",
            vec![Value::Temporal(TemporalValue::Interval {
                months: 1,
                days: 2,
                micros: 90_000_000_000,
            })],
            Value::Temporal(TemporalValue::Interval {
                months: 1,
                days: 3,
                micros: 3_600_000_000,
            }),
        ),
        (
            "isfinite",
            vec![Value::Temporal(TemporalValue::Date { days: 0 })],
            Value::Bool(true),
        ),
        (
            "uuid_extract_version",
            vec![Value::Str("019535d9-3df7-79fb-b466-fa907fa17f9e".into())],
            Value::Int(7),
        ),
        (
            "uuid_extract_timestamp",
            vec![Value::Str("00000000-0001-7000-8000-000000000000".into())],
            Value::Temporal(TemporalValue::TimestampTz { micros: 1_000 }),
        ),
    ]
}
