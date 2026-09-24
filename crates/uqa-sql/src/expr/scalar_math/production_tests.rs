//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn text(value: &str) -> Value {
    Value::Str(value.into())
}
fn numeric(value: &str) -> Value {
    Value::Decimal(DecimalValue::parse(value).unwrap())
}

fn controlled_result_cases() -> [(&'static str, Vec<Value>, Value); 21] {
    [
        (
            "lpad",
            vec![text("é"), Value::Int(4), text("界ø")],
            text("界ø界é"),
        ),
        ("rpad", vec![text("éab"), Value::Int(2)], text("éa")),
        ("lpad", vec![text("é"), Value::Int(4), text("")], text("é")),
        ("repeat", vec![text("é"), Value::Int(3)], text("ééé")),
        (
            "translate",
            vec![text("aéaø"), text("aéa"), text("界")],
            text("界界ø"),
        ),
        (
            "overlay",
            vec![text("aéøz"), text("界"), Value::Int(2), Value::Int(2)],
            text("a界z"),
        ),
        (
            "overlay",
            vec![text("ab"), text("é"), Value::Int(9)],
            text("abé"),
        ),
        (
            "split_part",
            vec![text("a::é::z"), text("::"), Value::Int(-2)],
            text("é"),
        ),
        (
            "split_part",
            vec![text("a"), text(""), Value::Int(-1)],
            text("a"),
        ),
        (
            "split_part",
            vec![text("a"), text("::"), Value::Int(i64::MIN)],
            text(""),
        ),
        (
            "trunc",
            vec![numeric("-129.999"), Value::Int(-1)],
            numeric("-120"),
        ),
        ("trunc", vec![numeric("-12.9")], numeric("-12")),
        ("trunc", vec![numeric("NaN")], numeric("NaN")),
        (
            "log",
            vec![Value::Int(10), Value::Int(100)],
            numeric("2.0000000000000000"),
        ),
        (
            "width_bucket",
            vec![Value::Int(7), Value::Int(10), Value::Int(0), Value::Int(5)],
            Value::Int(2),
        ),
        (
            "crc32",
            vec![Value::Bytes(b"123456789".to_vec())],
            Value::Int(0xcbf4_3926),
        ),
        (
            "crc32c",
            vec![Value::Bytes(b"123456789".to_vec())],
            Value::Int(0xe306_9283),
        ),
        (
            "encode",
            vec![Value::Bytes(vec![0, 255]), text("hex")],
            text("00ff"),
        ),
        (
            "encode",
            vec![Value::Bytes(vec![b'A', 255, b'\n']), text("escape")],
            text("A\\u{fffd}\\n"),
        ),
        (
            "decode",
            vec![text("61 \u{2003} 62"), text("hex")],
            Value::Bytes(b"ab".to_vec()),
        ),
        (
            "decode",
            vec![text("YWJj"), text("base64")],
            Value::Bytes(b"abc".to_vec()),
        ),
    ]
}

#[test]
fn controlled_math_preserves_unicode_decimal_encoding_split_and_checksum_results() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (name, args, expected) in controlled_result_cases() {
        let output = eval_math_functions_with_control(name, &args, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*output, &expected, "{name}");
        assert_eq!(
            eval_math_functions(name, &args).unwrap().unwrap(),
            expected,
            "{name}"
        );
        assert_eq!(budget.used(), output.reserved_bytes(), "{name}");
        drop(output);
        assert_eq!(budget.used(), 0, "{name}");
    }
}

#[test]
fn scalar_null_and_error_precedence_stays_shared_without_admitting_copies() {
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for name in ["atan2", "log", "trunc"] {
        let output =
            eval_math_functions_with_control(name, &[text("invalid"), Value::Null], &control)
                .unwrap()
                .unwrap();
        assert_eq!(&*output, &Value::Null);
        assert_eq!(output.reserved_bytes(), 0);
    }
    let output = eval_math_functions_with_control("sin", &[Value::Int(0)], &control)
        .unwrap()
        .unwrap();
    assert_eq!(&*output, &Value::Float(0.0));
    assert_eq!(
        super::super::conversion::to_f64_with_control(&Value::Bool(true), &control).unwrap(),
        1.0
    );
    for (name, args) in [
        ("sin", vec![]),
        (
            "width_bucket",
            vec![Value::Null, Value::Int(0), Value::Int(1), Value::Int(1)],
        ),
    ] {
        let ordinary = eval_math_functions(name, &args).unwrap().unwrap_err();
        let error = eval_math_functions_with_control(name, &args, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), ordinary.to_string());
    }
    assert!(eval_math_functions_with_control("format", &[text("%s")], &control).is_none());
    assert!(eval_math_functions_with_control("random", &[], &control).is_none());
    assert_eq!(
        eval_math_functions("format", &[text("%s:%d"), Value::Bool(true), Value::Int(7)])
            .unwrap()
            .unwrap(),
        text("t:7")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn quota_failures_never_become_numeric_fallbacks_or_base64_syntax_errors() {
    for limit in [0, 1, 8, 24, 64, 128] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (name, args) in [
            ("repeat", vec![text("x"), Value::Int(4096)]),
            ("lpad", vec![text("é"), Value::Int(4096), text("界")]),
            ("decode", vec![text(&"YQ==".repeat(1024)), text("base64")]),
            (
                "trunc",
                vec![
                    numeric("123456789012345678901234567890.123"),
                    Value::Int(2048),
                ],
            ),
        ] {
            let error = eval_math_functions_with_control(name, &args, &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"), "{name}: {error}");
            assert_eq!(budget.used(), 0, "{name}");
        }
    }
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let error =
        eval_math_functions_with_control("log", &[Value::Int(10), Value::Int(100)], &control)
            .unwrap()
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    let error =
        super::super::conversion::to_f64_with_control(&numeric("12.5"), &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn both_cancellation_scopes_stop_numeric_and_text_production() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (name, args) in [
            ("sin", vec![numeric("0")]),
            ("repeat", vec![text("x"), Value::Int(256)]),
            ("decode", vec![text("YQ=="), text("base64")]),
            ("crc32", vec![Value::Bytes(vec![0; 4096])]),
        ] {
            let error = eval_math_functions_with_control(name, &args, &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
}
