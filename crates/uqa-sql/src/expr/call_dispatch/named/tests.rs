//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};

fn named(name: &str, value: Value) -> (Option<String>, Value) {
    (Some(name.into()), value)
}

#[test]
fn named_scalar_inputs_preserve_declaration_order_and_omitted_defaults() {
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, args, expected) in [
        (
            "regexp_count",
            vec![
                named("pattern", Value::Str("a+".into())),
                named("string", Value::Str("baa".into())),
            ],
            vec![Value::Str("baa".into()), Value::Str("a+".into())],
        ),
        (
            "make_interval",
            vec![
                named("secs", Value::Float(1.5)),
                named("years", Value::Int(2)),
            ],
            vec![
                Value::Int(2),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Float(1.5),
            ],
        ),
        (
            "jsonb_strip_nulls",
            vec![named("target", Value::JsonB("{\"keep\":1}".into()))],
            vec![Value::JsonB("{\"keep\":1}".into()), Value::Bool(false)],
        ),
        (
            "array_sort",
            vec![
                named("descending", Value::Bool(true)),
                named(
                    "array",
                    Value::Array(ArrayValue::try_new(vec![Value::Int(2), Value::Int(1)]).unwrap()),
                ),
            ],
            vec![
                Value::Array(ArrayValue::try_new(vec![Value::Int(2), Value::Int(1)]).unwrap()),
                Value::Bool(true),
            ],
        ),
    ] {
        let original = args.clone();
        let result = builtin_named_args(name, &args, &control).unwrap().unwrap();
        assert_eq!(*result, expected, "{name}");
        assert_eq!(budget.used(), result.reserved_bytes());
        assert_eq!(args, original);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn malformed_named_slots_keep_the_existing_no_signature_result() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, args) in [
        (
            "regexp_count",
            vec![named("pattern", Value::Null), named("pattern", Value::Null)],
        ),
        (
            "regexp_count",
            vec![named("pattern", Value::Null), (None, Value::Null)],
        ),
        ("array_sort", vec![named("descending", Value::Bool(true))]),
        (
            "json_strip_nulls",
            vec![named("strip_in_arrays", Value::Bool(true))],
        ),
        ("make_interval", vec![named("unknown", Value::Int(1))]),
        ("unknown_function", vec![]),
    ] {
        assert!(
            builtin_named_args(name, &args, &control).unwrap().is_none(),
            "{name}"
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn reordered_argument_quota_failure_releases_partial_output_and_keeps_prior_owners() {
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("earlier result").unwrap();
    let used = budget.used();
    let args = [
        named("pattern", Value::Str("x".repeat(8192))),
        named("string", Value::Str("small".into())),
    ];
    let error = builtin_named_args("regexp_count", &args, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), used);
    assert_eq!(&*held, "earlier result");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn named_argument_production_checks_both_cancellation_scopes() {
    let budget = MemoryBudget::new(4096);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let args = [named("target", Value::JsonB("{\"a\":null}".into()))];
        assert_eq!(
            builtin_named_args("jsonb_strip_nulls", &args, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
