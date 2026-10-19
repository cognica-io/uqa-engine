//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{eval_collated_functions_with_control, eval_core_functions_with_control};
use uqa_core::{memory::Produced, Value};

/// The collation-dependent family runs under the database default collation; every other core function keeps its own dispatch.
fn core(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<crate::error::Result<Produced<Value>>> {
    eval_collated_functions_with_control(
        name,
        args,
        crate::catalog::collations::DEFAULT_COLLATION_OID as u32,
        control,
    )
    .or_else(|| eval_core_functions_with_control(name, args, control))
}

fn core_ordinary(name: &str, args: &[Value]) -> Option<crate::error::Result<Value>> {
    core(name, args, &ProductionControl::uncontrolled())
        .map(|result| result.map(|value| value.into_uncontrolled().expect("ordinary core result")))
}

#[test]
fn like_and_ilike_propagate_null_arguments() {
    for name in ["like", "ilike"] {
        for arguments in [
            vec![Value::Null, Value::Str("%".into())],
            vec![Value::Str("text".into()), Value::Null],
            vec![
                Value::Str("text".into()),
                Value::Str("%".into()),
                Value::Null,
            ],
        ] {
            assert_eq!(
                core_ordinary(name, &arguments).unwrap().unwrap(),
                Value::Null
            );
        }
    }
}

#[test]
fn prefix_comparison_borrows_inputs_and_preserves_both_cancellation_scopes() {
    let memory = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let input = "한글".repeat(8192);
    let arguments = [text(&input), text(&input)];
    for collation in [100, 811, 950, 951, 962, 963, 6411] {
        for (source, prefix, expected) in [
            ("abc", "a", true),
            ("abc", "b", false),
            ("abc", "", true),
            ("", "a", false),
            ("", "", true),
            ("한글문자", "한글", true),
            ("éabc", "e", false),
            ("abc", "a%", false),
            ("abc", "A", false),
        ] {
            assert_eq!(
                *eval_collated_functions_with_control(
                    "starts_with",
                    &[text(source), text(prefix)],
                    collation,
                    &control,
                )
                .unwrap()
                .unwrap(),
                Value::Bool(expected)
            );
        }
        assert_eq!(
            *eval_collated_functions_with_control("starts_with", &arguments, collation, &control)
                .unwrap()
                .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error = eval_collated_functions_with_control("starts_with", &arguments, 100, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, DecimalValue,
};

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn assert_value(name: &str, args: &[Value], expected: Value) {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let result = core(name, args, &control).unwrap().unwrap();
    assert_eq!(&*result, &expected, "{name}");
    assert_eq!(
        memory.used(),
        result.reserved_bytes(),
        "{name} released scratch"
    );
    match &*result {
        Value::Str(value) => assert_eq!(memory.used(), value.capacity()),
        Value::Bytes(value) => assert_eq!(memory.used(), value.capacity()),
        _ => {}
    }
    assert_eq!(core_ordinary(name, args).unwrap().unwrap(), expected);
    drop(result);
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_text_functions_keep_character_byte_and_boundary_semantics() {
    let cases = [
        ("upper", vec![text("Straße")], text("STRASSE")),
        ("lower", vec![text("ΟΣ")], text("ος")),
        ("casefold", vec![text("Straße ΟΣ")], text("strasse οσ")),
        ("initcap", vec![text("hELLO\tWÖRLD")], text("Hello\tWörld")),
        ("reverse", vec![text("aé中")], text("中éa")),
        (
            "reverse",
            vec![Value::Bytes(vec![0, 255, 2])],
            Value::Bytes(vec![2, 255, 0]),
        ),
        (
            "length",
            vec![Value::FixedChar("é  ".into())],
            Value::Int(1),
        ),
        (
            "octet_length",
            vec![Value::FixedChar("é  ".into())],
            Value::Int(4),
        ),
        ("trim", vec![text("中aé中"), text("é中")], text("a")),
        ("ltrim", vec![text("  x  ")], text("x  ")),
        ("rtrim", vec![text("  x  ")], text("  x")),
        ("trim", vec![text("\t \n")], text("")),
        (
            "replace",
            vec![text("éa"), text(""), text("-")],
            text("-é-a-"),
        ),
        (
            "replace",
            vec![text("aaaaa"), text("aa"), text("x")],
            text("xxa"),
        ),
        (
            "substring",
            vec![text("é中abc"), Value::Int(-1), Value::Int(4)],
            text("é中"),
        ),
        ("substr", vec![text("é中abc"), Value::Int(3)], text("abc")),
        ("left", vec![text("é中abc"), Value::Int(-2)], text("é中a")),
        ("right", vec![text("é中abc"), Value::Int(-2)], text("abc")),
        ("left", vec![text("é中"), Value::Int(0)], text("")),
        (
            "starts_with",
            vec![text("é中abc"), text("é中")],
            Value::Bool(true),
        ),
        ("strpos", vec![text("é中abc"), text("a")], Value::Int(3)),
        ("ascii", vec![text("é")], Value::Int(233)),
        ("chr", vec![Value::Int(233)], text("é")),
        ("concat_op", vec![text("a"), Value::Int(1)], text("a1")),
        ("like", vec![text("é中"), text("__")], Value::Bool(true)),
        ("ilike", vec![text("ΟΣ"), text("ος")], Value::Bool(true)),
    ];
    for (name, args, expected) in cases {
        assert_value(name, &args, expected);
    }
}

#[test]
fn controlled_selection_and_numeric_functions_keep_existing_results() {
    assert_value(
        "coalesce",
        &[Value::Null, text("selected"), text("later")],
        text("selected"),
    );
    assert_value("nullif", &[text("a"), text("a")], Value::Null);
    assert_value("nullif", &[text("a"), Value::Null], text("a"));
    assert_value("greatest", &[text("b"), Value::Null, text("a")], text("b"));
    assert_value(
        "least",
        &[Value::Int(2), Value::Null, Value::Int(1)],
        Value::Int(1),
    );
    for (name, args, expected) in [
        ("abs", vec![Value::Int(-3)], Value::Int(3)),
        ("ceil", vec![Value::Float(1.25)], Value::Float(2.0)),
        ("floor", vec![Value::Float(-1.25)], Value::Float(-2.0)),
        ("round", vec![Value::Float(2.5)], Value::Float(2.0)),
        (
            "power",
            vec![Value::Int(2), Value::Int(3)],
            Value::Float(8.0),
        ),
        ("sqrt", vec![Value::Float(9.0)], Value::Float(3.0)),
        (
            "mod",
            vec![Value::Int(i64::MIN), Value::Int(-1)],
            Value::Int(0),
        ),
        ("div", vec![Value::Int(7), Value::Int(2)], Value::Int(3)),
        ("gcd", vec![Value::Int(6), Value::Int(9)], Value::Int(3)),
        ("lcm", vec![Value::Int(6), Value::Int(9)], Value::Int(18)),
    ] {
        assert_value(name, &args, expected);
    }
    let decimal = |text: &str| Value::Decimal(DecimalValue::parse(text).unwrap());
    assert_value("round", &[decimal("2.50")], decimal("3"));
    assert_value("mod", &[decimal("7.5"), decimal("2")], decimal("1.5"));
    assert_value("power", &[decimal("2"), decimal("3")], decimal("8"));
}

#[test]
fn controlled_core_failures_release_scratch_and_preserve_other_owners() {
    for limit in [0, 64, 256] {
        let memory = MemoryBudget::new(limit + 16);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let retained = control.copy_text("prior").unwrap();
        let used = memory.used();
        for (name, args) in [
            ("upper", vec![text(&"ß".repeat(512))]),
            (
                "replace",
                vec![text(&"a".repeat(512)), text("a"), text("expanded")],
            ),
            ("like", vec![text("a"), text(&"%a".repeat(512))]),
        ] {
            let error = core(name, &args, &control).unwrap().unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"), "{name}");
            assert_eq!(memory.used(), used);
        }
        assert_eq!(retained.as_str(), "prior");
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        for (name, args) in [
            ("coalesce", vec![Value::Null]),
            ("lower", vec![text("A")]),
            ("abs", vec![Value::Int(1)]),
            ("like", vec![text("a"), text("a")]),
        ] {
            assert_eq!(
                core(name, &args, &control).unwrap().unwrap_err().sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn controlled_core_preserves_error_precedence_and_zero_payload_results() {
    let memory = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (name, args, state) in [
        (
            "substring",
            vec![text("abc"), Value::Int(1), Value::Int(-1)],
            "22011",
        ),
        (
            "substring",
            vec![text("abc"), Value::Int(i64::MAX), Value::Int(1)],
            "22003",
        ),
        ("like", vec![Value::Null, text("a"), text("ab")], "22025"),
        ("mod", vec![Value::Int(1), Value::Int(0)], "22012"),
    ] {
        assert_eq!(
            core(name, &args, &control).unwrap().unwrap_err().sqlstate(),
            Some(state)
        );
        assert_eq!(memory.used(), 0);
    }
    let zero = MemoryBudget::new(0);
    let control = ProductionControl::new(&zero, &token, &token);
    assert_eq!(
        core("upper", &[text("a")], &control)
            .unwrap()
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(zero.used(), 0);
    for (name, args, expected) in [
        ("length", vec![text("é")], Value::Int(1)),
        ("coalesce", vec![Value::Null], Value::Null),
        ("abs", vec![Value::Int(-1)], Value::Int(1)),
    ] {
        assert_eq!(&*core(name, &args, &control).unwrap().unwrap(), &expected);
        assert_eq!(zero.used(), 0);
    }
}

#[test]
fn selection_uses_record_equality_and_total_field_null_order() {
    // Independent cases: composite_expression_comparisons_oracle on PostgreSQL 18.4.
    let missing = Value::Row(vec![Value::Int(1), Value::Null].into());
    let present = Value::Row(vec![Value::Int(1), Value::Int(2)].into());
    assert_value("nullif", &[missing.clone(), missing.clone()], Value::Null);
    for arguments in [
        vec![missing.clone(), present.clone()],
        vec![present.clone(), missing.clone()],
    ] {
        assert_value("least", &arguments, present.clone());
        assert_value("greatest", &arguments, missing.clone());
    }
}

/// Strict calls never reach a provider for a NULL input, so an indeterminate collation is reported only for a value the function maps or matches, as `PostgreSQL` 18.4 reports it.
#[test]
fn an_indeterminate_collation_is_reported_only_for_values_that_need_it() {
    let control = ProductionControl::uncontrolled();
    for (name, arguments, subject) in [
        ("lower", vec![text("A")], "lower() function"),
        ("casefold", vec![text("A")], "lower() function"),
        ("upper", vec![text("a")], "upper() function"),
        ("initcap", vec![text("a")], "initcap() function"),
        ("like", vec![text("a"), text("a")], "LIKE"),
        ("ilike", vec![text("a"), text("a")], "ILIKE"),
        ("ilike", vec![text("a"), text("a"), text("#")], "ILIKE"),
        (
            "starts_with",
            vec![text("abc"), text("a")],
            "string comparison",
        ),
        ("starts_with", vec![text(""), text("")], "string comparison"),
        (
            "starts_with",
            vec![text("a"), text("longer")],
            "string comparison",
        ),
    ] {
        let error = eval_collated_functions_with_control(name, &arguments, 0, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P22"), "{name}");
        assert_eq!(
            error.to_string(),
            format!("could not determine which collation to use for {subject}")
        );
        let mut absent = arguments.clone();
        absent[0] = Value::Null;
        assert_eq!(
            *eval_collated_functions_with_control(name, &absent, 0, &control)
                .unwrap()
                .unwrap(),
            Value::Null,
            "{name}"
        );
    }
    // The escape rewrite precedes the strict operator, so an invalid escape is reported before a NULL input or the collation.
    let error = eval_collated_functions_with_control(
        "ilike",
        &[Value::Null, text("a"), text("ab")],
        0,
        &control,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22025"));
    // Every other function keeps its own dispatch.
    assert!(eval_collated_functions_with_control("reverse", &[text("a")], 0, &control).is_none());
}

#[test]
fn case_mapping_follows_the_input_collation() {
    let control = ProductionControl::uncontrolled();
    let mapped = |name: &str, input: &str, collation: u32| {
        eval_collated_functions_with_control(name, &[text(input)], collation, &control)
            .unwrap()
            .unwrap()
            .into_uncontrolled()
            .unwrap()
    };
    assert_eq!(mapped("upper", "stra\u{df}e", 950), text("STRA\u{df}E"));
    assert_eq!(mapped("upper", "stra\u{df}e", 811), text("STRA\u{df}E"));
    assert_eq!(mapped("upper", "stra\u{df}e", 6411), text("STRASSE"));
    assert_eq!(mapped("upper", "stra\u{df}e", 963), text("STRASSE"));
    assert_eq!(mapped("lower", "\u{c9}A", 950), text("\u{c9}a"));
    assert_eq!(mapped("lower", "\u{c9}A", 811), text("\u{e9}a"));
    assert_eq!(
        mapped("initcap", "o'neil \u{1c6}x", 950),
        text("O'Neil \u{1c6}X")
    );
    assert_eq!(
        mapped("initcap", "o'neil \u{1c6}x", 811),
        text("O'Neil \u{1c4}x")
    );
    assert_eq!(
        mapped("initcap", "o'neil \u{1c6}x", 6411),
        text("O'Neil \u{1c5}x")
    );
    assert_eq!(
        mapped("initcap", "o'neil \u{1c6}x", 963),
        text("O'neil \u{1c5}x")
    );
    assert_eq!(mapped("casefold", "\u{df}", 6411), text("ss"));
    assert_eq!(mapped("casefold", "\u{df}", 811), text("\u{df}"));
}
