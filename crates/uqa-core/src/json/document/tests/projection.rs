//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn compare_projected(text: &str) {
    let source: serde_json::Value = serde_json::from_str(text).unwrap();
    let expected = serde_json::from_value::<serde_json::Value>(source);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let source = JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
    let result = crate::json::work::inline(source.root().project_with_work(
        &control,
        &mut (),
        protocol(),
    ));
    match (expected, result) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            actual
                .root()
                .to_json_with_control(&control)
                .unwrap()
                .as_str(),
            expected.to_string(),
            "{text}"
        ),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data(), "{text}");
            assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text}");
        }
        (expected, actual) => panic!(
            "projection differs for {text}: {expected:?}, success {}",
            actual.is_ok()
        ),
    }
    drop(source);
    assert_eq!(memory.used(), 0);
}

#[test]
fn generic_document_projection_preserves_sorted_scalar_fields_and_failure_order() {
    for key in [protocol().raw, protocol().number] {
        for payload in [
            "1", "-0", "1.00", "0.000001", "1e309", "01", "[1,]", "[]", "{}", "\"text\"",
        ] {
            for position in ["root", "array", "object"] {
                let value = format!("{{\"z\":null,{key:?}:{payload:?}}}");
                let text = match position {
                    "array" => format!("[{value}]"),
                    "object" => format!("{{\"n\":{value}}}"),
                    _ => value,
                };
                compare_projected(&text);
            }
        }
        for payload in ["null", "true", "1", "[]", "{}"] {
            compare_projected(&format!("{{\"z\":null,{key:?}:{payload}}}"));
        }
    }
    compare_projected(r#"{"z":[1,{"n":2}],"a":["한🙂\u0000",false]}"#);
    let inner = format!("{}null{}", "[".repeat(126), "]".repeat(126));
    compare_projected(&format!(
        "{}{{{:?}:{inner:?}}}{}",
        "[".repeat(126),
        protocol().raw,
        "]".repeat(126)
    ));
}

#[test]
fn generic_document_projection_preserves_integer_widths_and_float_visitor_spelling() {
    for mantissa in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1",
        "1.0",
        "1.00",
        "18446744073709551616",
        "340282366920938463463374607431768211455",
        "340282366920938463463374607431768211456",
        "-170141183460469231731687303715884105728",
        "0.000001",
    ] {
        for suffix in ["", "e0", "e+0000", "e-3", "e40", "e309", "e-999999"] {
            compare_projected(&format!("{mantissa}{suffix}"));
        }
    }
    for power in [39, 40, 45, 100, 308, 309, 511, 512, 1024] {
        compare_projected(&format!("1{}", "0".repeat(power)));
    }
    for value in [
        f64::MIN,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        1e-6,
        -1e-6,
        1e40,
        -1e40,
        1e-100,
    ] {
        compare_projected(&value.to_string());
        compare_projected(&serde_json::to_string(&value).unwrap());
    }
    let mut bits = 0x1234_5678_9abc_def0u64;
    for _ in 0..256 {
        bits = bits.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let value = f64::from_bits(bits);
        if value.is_finite() {
            compare_projected(&value.to_string());
            compare_projected(&serde_json::to_string(&value).unwrap());
        }
    }
}

#[test]
fn generic_document_typed_unsigned_preserves_unlocated_value_diagnostics() {
    for text in [
        "null",
        "true",
        "[]",
        "{}",
        "\"한🙂\"",
        "-1",
        "-0",
        "0",
        "1.0",
        "1e0",
        "4294967295",
        "4294967296",
        "1e999999",
    ] {
        let generic: serde_json::Value = serde_json::from_str(text).unwrap();
        let expected = serde_json::from_value::<u32>(generic);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let value = JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
        match (
            expected,
            crate::json::work::inline(value.root().read_u32_with_work(&control, &mut ())),
        ) {
            (Ok(expected), Ok(actual)) => assert_eq!(actual, expected),
            (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
                assert_eq!(actual.to_string(), expected.to_string(), "{text}");
                assert_eq!(actual.is_syntax(), expected.is_syntax());
                assert_eq!(actual.is_data(), expected.is_data());
                assert_eq!(actual.position().line, expected.line());
                assert_eq!(actual.position().column, expected.column());
            }
            _ => panic!("typed unsigned differs for {text}"),
        }
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn generic_document_projection_accounts_for_actual_owned_allocations() {
    let text = format!(
        r#"{{"a":"{}","b":[1.0,0.000001,{{"field":"value"}}]}}"#,
        "한🙂".repeat(8192)
    );
    let source_memory = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&source_memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            crate::json::work::inline(source.root().project_with_work(
                &control,
                &mut (),
                protocol(),
            ))
            .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(measured.bytes_max).unwrap() <= memory.peak());
    drop(source);
    assert_eq!(source_memory.used(), 0);
    let projected = output.unwrap();
    assert_eq!(
        projected.root().get("a").unwrap().as_str().unwrap().len(),
        "한🙂".len() * 8192
    );
    drop(projected);
    assert_eq!(memory.used(), 0);
}
