//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue};

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
