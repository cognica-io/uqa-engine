//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn canonical_text_preserves_numeric_normalization_and_releases_conversion_scratch() {
    let memory = MemoryBudget::new(8 * 1024 * 1024);
    let cancellation = CancellationToken::new();
    let mut inputs = [
        "0",
        "-0.000",
        "1.000",
        "-12.3400",
        "0.0000012000",
        "1000000000",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e-16383",
        "1e131071",
    ]
    .map(str::to_string)
    .to_vec();
    // Exercise both sides of num-bigint's 64-limb radix conversion threshold on 32- and 64-bit hosts, plus its largest accepted precision.
    for digits in [607, 608, 616, 1224, 1234, 2500, 131_072] {
        inputs.push("9".repeat(digits));
    }
    for input in inputs {
        let value = DecimalValue::parse(&input).unwrap();
        let result = value
            .to_canonical_string_budgeted(&memory, &cancellation)
            .unwrap();
        assert_eq!(&*result, value.to_canonical_string());
        assert_eq!(memory.used(), result.capacity());
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn canonical_format_checks_quota_before_radix_expansion_and_preserves_the_input() {
    let value = DecimalValue::parse("1e131071").unwrap();
    let memory = MemoryBudget::new(128);
    let error = value
        .to_canonical_string_budgeted(&memory, &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        ValueRetentionError::Memory(MemoryError::Limit { .. })
    ));
    assert_eq!(memory.used(), 0);
    assert_eq!(memory.peak(), 0);
    assert_eq!(value.display_scale(), Some(0));
}

#[test]
fn cancelled_canonical_format_returns_the_original_typed_error_without_retention() {
    let value = DecimalValue::parse("12.3400").unwrap();
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        value.to_canonical_string_budgeted(&memory, &cancellation),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn produced_sql_text_preserves_display_scale_and_releases_both_workspaces() {
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for input in [
        "0",
        "0.0000",
        "12.3400",
        "-0.0000012000",
        "10000",
        "NaN",
        "Infinity",
        "-Infinity",
    ] {
        let value = DecimalValue::parse_with_control(input, &control)
            .unwrap()
            .unwrap();
        let retained = value.reserved_bytes();
        let text = value.to_sql_string_with_control(&control).unwrap();
        assert_eq!(&**text, input);
        assert_eq!(memory.used(), retained + text.capacity());
        drop(text);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    invoking.cancel();
    assert!(matches!(
        DecimalValue::parse_with_control("1", &control),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
