//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn run(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    evaluate(name, args, control).expect("known regular-expression family")
}

#[test]
fn regex_outputs_keep_capture_nulls_unicode_offsets_and_replacement_selection() {
    let budget = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, args, expected) in [
        (
            "regexp_match",
            vec![text("ab12"), text(r"([a-z]+)(\d+)(z)?")],
            Value::Array(ArrayValue::try_new(vec![text("ab"), text("12"), Value::Null]).unwrap()),
        ),
        (
            "regexp_match",
            vec![text("éab"), text("ab")],
            Value::Array(ArrayValue::try_new(vec![text("ab")]).unwrap()),
        ),
        ("regexp_count", vec![text("éx"), text("")], Value::Int(3)),
        (
            "regexp_instr",
            vec![
                text("é1 α22"),
                text(r"(\d+)"),
                Value::Int(1),
                Value::Int(2),
                Value::Int(1),
                text(""),
                Value::Int(1),
            ],
            Value::Int(7),
        ),
        (
            "regexp_substr",
            vec![
                text("a b"),
                text("([ab])(z)?"),
                Value::Int(1),
                Value::Int(2),
                text(""),
                Value::Int(2),
            ],
            Value::Null,
        ),
        (
            "regexp_like",
            vec![text("한\n글"), text("한.글")],
            Value::Bool(true),
        ),
        (
            "regexp_replace",
            vec![
                text("ab12 cd34"),
                text(r"([a-z]+)(\d+)"),
                text(r"\2-\1-$"),
                text("g"),
            ],
            text("12-ab-$ 34-cd-$"),
        ),
        (
            "regexp_replace",
            vec![
                text("éa a a"),
                text("a"),
                text("X"),
                Value::Int(2),
                Value::Int(2),
            ],
            text("éa X a"),
        ),
        (
            "similar_to",
            vec![text("ab\ncd"), text("a%d")],
            Value::Bool(true),
        ),
    ] {
        let result = run(name, &args, &control).unwrap();
        assert_eq!(*result, expected, "{name}");
        assert_eq!(
            budget.used(),
            result.reserved_bytes(),
            "only the SQL result survives"
        );
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn regex_failure_order_preserves_native_errors_and_outside_start_behavior() {
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, args, state) in [
        (
            "regexp_like",
            vec![text("a"), text("["), text("z")],
            "22023",
        ),
        ("regexp_like", vec![text("a"), text("[")], "2201B"),
        (
            "regexp_count",
            vec![text("a"), text("["), Value::Int(0)],
            "22023",
        ),
        (
            "regexp_instr",
            vec![
                text("a"),
                text("a"),
                Value::Int(1),
                Value::Int(1),
                Value::Int(-1),
            ],
            "22023",
        ),
    ] {
        assert_eq!(
            run(name, &args, &control).unwrap_err().sqlstate(),
            Some(state)
        );
        assert_eq!(budget.used(), 0);
    }
    let result = run(
        "regexp_replace",
        &[text("a"), text("["), text("x"), Value::Int(3)],
        &control,
    )
    .unwrap();
    assert_eq!(*result, text("a"));
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn regex_output_quota_failure_releases_partial_replacements_and_keeps_earlier_values() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("earlier value").unwrap();
    let retained = budget.used();
    let args = [
        text(&"a".repeat(64)),
        text("a"),
        text(&"x".repeat(1024)),
        text("g"),
    ];
    assert_eq!(
        run("regexp_replace", &args, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), retained);
    assert_eq!(&*held, "earlier value");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn regex_production_observes_both_cancellation_scopes() {
    let budget = MemoryBudget::new(8192);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let held = run("regexp_substr", &[text("before"), text(".*")], &control).unwrap();
        let retained = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            run("regexp_match", &[text("later"), text(".*")], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), retained);
        assert_eq!(*held, text("before"));
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}
