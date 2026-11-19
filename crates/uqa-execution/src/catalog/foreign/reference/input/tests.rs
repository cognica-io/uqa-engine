//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryBudget;

fn compare(text: &str) {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let expected = serde_json::from_str::<serde_json::Value>(text).map(|value| {
        for version in [1, 2] {
            if value == serde_json::json!({"version":version}) {
                return Shape::Marker(version);
            }
            if value == serde_json::json!(version) {
                return Shape::Integer(version);
            }
        }
        Shape::Other
    });
    let actual = observe(text, control);
    match expected {
        Ok(expected) => assert_eq!(actual.unwrap(), expected, "{text}"),
        Err(_) => assert!(
            matches!(actual, Err(StorageBackendError::Serde(_))),
            "{text}: {actual:?}"
        ),
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn foreign_reference_marker_preserves_json_value_carriers_and_duplicate_semantics() {
    for text in [
        "null",
        "[]",
        "{}",
        "1",
        "2",
        "-0",
        "2.0",
        "1e99999",
        r#"{"version":1}"#,
        r#"{"version":2}"#,
        r#"{"version":2.0}"#,
        r#"{"version":null,"version":2}"#,
        r#"{"version":2,"version":null}"#,
        r#"{"version":2,"extra":1}"#,
        r#"{"extra":1,"version":2}"#,
        r#"{"version":{"$serde_json::private::Number":"2"}}"#,
        r#"{"version":{"$serde_json::private::Number":"2.0"}}"#,
        r#"{"version":{"$serde_json::private::Number":" 2"}}"#,
        r#"{"version":{"$serde_json::private::Number":"2 "}}"#,
        r#"{"version":{"$serde_json::private::Number":"[]"}}"#,
        r#"{"version":{"$serde_json::private::Number":2}}"#,
        r#"{"version":{"$serde_json::private::Number":"2","extra":1}}"#,
        r#"{"$serde_json::private::RawValue":"{\"version\":2}"}"#,
        r#"{"version":{"$serde_json::private::RawValue":" 2 "}}"#,
        r#"{"version":{"$serde_json::private::RawValue":"2 3"}}"#,
        r#"{"version":{"$serde_json::private::RawValue":{}}}"#,
        r#"{"version":{"$serde_json::private::RawValue":"2","extra":1}}"#,
        r#"{"ignored":{"$serde_json::private::Number":"bad"},"version":2}"#,
        r#"{"version":{"x":1,"$serde_json::private::Number":"bad"},"version":2}"#,
        r#"{"version":{"$serde_json::private::RawValue":"bad"},"version":2}"#,
        r#"{"version":"\uD800","version":2}"#,
    ] {
        compare(text);
    }
    for depth in [16, 126, 127, 128, 129] {
        compare(&format!("{}2{}", "[".repeat(depth), "]".repeat(depth)));
    }
    let mut raw = r#"{"version":2}"#.to_owned();
    for _ in 0..12 {
        raw = format!(
            "{{\"$serde_json::private::RawValue\":{}}}",
            serde_json::to_string(&raw).unwrap()
        );
        compare(&raw);
    }
}

#[test]
fn foreign_reference_marker_matches_original_truncations_and_byte_mutations() {
    for text in [
        r#"{"version":2}"#,
        r#"{"version":{"$serde_json::private::Number":"2"}}"#,
        r#"{"$serde_json::private::RawValue":"{\"version\":2}"}"#,
    ] {
        for end in 0..=text.len() {
            compare(&text[..end]);
        }
        for position in 0..text.len() {
            for byte in b"01nt{}[],:\\\" " {
                let mut bytes = text.as_bytes().to_vec();
                bytes[position] = *byte;
                compare(std::str::from_utf8(&bytes).unwrap());
            }
        }
    }
}

#[test]
fn foreign_reference_marker_bounds_discarded_values_and_retires_rejected_input() {
    let text = format!("{{\"version\":{},\"version\":2}}", "9".repeat(2 << 20));
    let memory = MemoryBudget::new(8192);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let measured = allocation_counter::measure(|| {
        assert_eq!(observe(&text, control).unwrap(), Shape::Marker(2));
    });
    assert!(measured.bytes_max < 8192, "{measured:?}");
    assert_eq!(memory.used(), 0);
    let nested = format!("{}2{}", "[".repeat(70), "]".repeat(70));
    observe(&nested, control).unwrap();
    let peak = memory.peak();
    let mut denied = 0;
    let mut admitted = 0;
    // Tight allowances select exact buffer growth instead of the preferred doubled capacity.
    for limit in (0..peak).step_by(17).chain([peak - 1, peak]) {
        let budget = MemoryBudget::new(limit);
        match observe(
            &nested,
            ProductionControl::new(&budget, &original, &invoking),
        ) {
            Ok(value) => {
                assert_eq!(value, Shape::Other);
                admitted += 1;
            }
            Err(error) => {
                assert!(matches!(error, StorageBackendError::Memory(_)));
                denied += 1;
            }
        }
        assert!(budget.peak() <= limit);
        assert_eq!(budget.used(), 0);
    }
    assert!(denied > 0 && admitted > 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(observe(&text, ProductionControl::new(&memory, &original, &invoking)).is_err());
        assert_eq!(memory.used(), 0);
    }
}
