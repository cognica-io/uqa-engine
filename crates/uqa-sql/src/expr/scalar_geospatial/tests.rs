//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue, TemporalValue};

fn evaluate(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Produced<Value> {
    eval_geospatial_functions_with_control(name, args, control)
        .unwrap()
        .unwrap()
}

#[test]
fn point_output_retains_its_buffer_and_scalar_predicates_release_workspace() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let point = evaluate("point", &[Value::Int(3), Value::Float(4.0)], &control);
    assert_eq!(
        *point,
        Value::List(vec![Value::Float(3.0), Value::Float(4.0)])
    );
    assert!(point.reserved_bytes() >= 2 * size_of::<Value>());
    assert_eq!(budget.used(), point.reserved_bytes());
    let retained = budget.used();
    let origin = Value::Str("(0, 0)".into());
    for (name, args, expected) in [
        (
            "st_distance",
            vec![origin.clone(), (*point).clone()],
            Value::Float(5.0),
        ),
        (
            "st_within",
            vec![origin.clone(), origin.clone()],
            Value::Bool(true),
        ),
        (
            "st_dwithin",
            vec![origin, (*point).clone(), Value::Int(4)],
            Value::Bool(false),
        ),
        (
            "overlaps",
            vec![
                Value::Str("2026-01-01 00:00:00".into()),
                Value::Str("2026-01-03 00:00:00".into()),
                Value::Str("2026-01-02 00:00:00".into()),
                Value::Str("2026-01-04 00:00:00".into()),
            ],
            Value::Bool(true),
        ),
    ] {
        let output = evaluate(name, &args, &control);
        assert_eq!(*output, expected, "{name}");
        assert_eq!(output.reserved_bytes(), 0);
        assert_eq!(budget.used(), retained);
    }
    drop(point);
    assert_eq!(budget.used(), 0);
}

#[test]
fn point_parsing_borrows_text_but_decimal_conversion_and_outputs_require_allowance() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        *evaluate(
            "st_distance",
            &[Value::Str("[0, 0]".into()), Value::Str("(3,4)".into())],
            &control
        ),
        Value::Float(5.0)
    );
    for (name, args) in [
        ("point", vec![Value::Int(1), Value::Int(2)]),
        (
            "st_distance",
            vec![
                Value::List(vec![
                    Value::Decimal(DecimalValue::parse("1.25").unwrap()),
                    Value::Int(2),
                ]),
                Value::Str("(0,0)".into()),
            ],
        ),
    ] {
        let error = eval_geospatial_functions_with_control(name, &args, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
    for invalid in ["(1,2,3)", "(1)", "(a,2)"] {
        assert!(eval_geospatial_functions_with_control(
            "st_distance",
            &[Value::Str(invalid.into()), Value::Str("(0,0)".into())],
            &control
        )
        .unwrap()
        .is_err());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn geospatial_producers_observe_original_and_invoking_cancellation() {
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
        for name in [
            "point",
            "st_distance",
            "st_within",
            "st_dwithin",
            "overlaps",
        ] {
            let error = eval_geospatial_functions_with_control(
                name,
                &[Value::Int(0), Value::Int(0)],
                &control,
            )
            .unwrap()
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
        assert!(eval_geospatial_functions_with_control("unrelated", &[], &control).is_none());
    }
}

#[test]
fn overlaps_compares_typed_endpoints_at_postgresql_precision() {
    let control = ProductionControl::uncontrolled();
    let ts = |s| Value::Temporal(TemporalValue::parse_timestamp(s).unwrap());
    let anchor = ts("2000-01-01");
    let next = ts("2000-01-02");
    let end = ts("2000-01-03");
    // PostgreSQL 18.4 OVERLAPS resolves reversed, instantaneous and unknown periods.
    for (args, expected) in [
        (
            [next.clone(), anchor.clone(), anchor.clone(), end.clone()],
            Value::Bool(true),
        ),
        (
            [anchor.clone(), anchor.clone(), anchor.clone(), next.clone()],
            Value::Bool(true),
        ),
        (
            [anchor.clone(), next.clone(), next.clone(), next.clone()],
            Value::Bool(false),
        ),
        (
            [next.clone(), Value::Null, anchor.clone(), end.clone()],
            Value::Bool(true),
        ),
        (
            [end, Value::Null, anchor.clone(), next.clone()],
            Value::Null,
        ),
        (
            [Value::Null, Value::Null, anchor.clone(), next],
            Value::Null,
        ),
        (
            [
                Value::Str("1999-12-31 19:00:00-05".into()),
                Value::Str("2000-01-01T00:00:00.0000004Z".into()),
                Value::Temporal(TemporalValue::TimestampTz { postgres_micros: 0 }),
                Value::Temporal(TemporalValue::TimestampTz { postgres_micros: 0 }),
            ],
            Value::Bool(true),
        ),
    ] {
        assert_eq!(*evaluate("overlaps", &args, &control), expected);
    }
}

#[test]
fn overlaps_observes_retained_temporal_fields_when_their_values_are_needed() {
    use uqa_core::DatumValue;
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let start = TemporalValue::parse_timestamp("2000-01-01").unwrap();
    let day = TemporalValue::parse_interval("1 day").unwrap();
    let last_hour = Value::Temporal(TemporalValue::parse_timestamp("2000-01-01 23:00").unwrap());
    let midnight = Value::Temporal(TemporalValue::parse_timestamp("2000-01-02").unwrap());
    let physical = |value: &TemporalValue, oid: u32| {
        let (bytes, len) =
            crate::catalog::node_tree::encode_temporal_datum_inline(value, i64::from(oid)).unwrap();
        Value::Datum(DatumValue::new(oid, 0, bytes[..len].to_vec()))
    };
    for (start, duration) in [
        (physical(&start, 1114), physical(&day, 1186)),
        (Value::Temporal(start), Value::Temporal(day)),
    ] {
        assert_eq!(
            *evaluate(
                "overlaps",
                &[start, duration, last_hour.clone(), midnight.clone()],
                &control
            ),
            Value::Bool(true)
        );
    }
    let unread = Value::Datum(DatumValue::new(1186, 0, Vec::new()));
    assert_eq!(
        *evaluate(
            "overlaps",
            &[Value::Null, unread, last_hour, midnight],
            &control
        ),
        Value::Null
    );
    assert_eq!(budget.used(), 0);
}
