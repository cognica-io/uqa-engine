//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn admitted_quantization_preserves_midpoints_negative_scale_and_display_scale() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (input, scale, expected) in [
        ("12.345", 2, "12.35"),
        ("-12.345", 2, "-12.35"),
        ("99.995", 2, "100.00"),
        ("0.009", 2, "0.01"),
        ("0.0049", 2, "0.00"),
        ("-0.005", 2, "-0.01"),
        ("12", 4, "12.0000"),
        ("150", -2, "200"),
        ("-150", -2, "-200"),
        ("49", -2, "0"),
        ("50", -2, "100"),
        ("500", -4, "0"),
        ("0", 3, "0.000"),
        ("NaN", 2, "NaN"),
        ("Infinity", 2, "Infinity"),
    ] {
        let input = DecimalValue::parse(input).unwrap();
        let output = input
            .round_to_scale_with_control(scale, &control)
            .unwrap()
            .unwrap();
        assert_eq!(output.to_sql_string(), expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    for (input, precision, scale, expected) in [
        ("123.40", 5, 2, true),
        ("123.40", 4, 2, false),
        ("0.001", 1, 3, true),
        ("1200", 2, -2, true),
        ("0", 0, 0, false),
    ] {
        assert_eq!(
            DecimalValue::parse(input)
                .unwrap()
                .fits_precision_with_control(precision, scale, &control)
                .unwrap(),
            expected
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn numeric_primitives_and_failed_quantization_keep_ownership_exact() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        let output = DecimalValue::from_i64_with_control(value, &control).unwrap();
        assert_eq!(
            output.to_i64_trunc_with_control(&control).unwrap(),
            Some(value)
        );
        assert_eq!(budget.used(), output.retained_bytes());
        drop(output);
    }
    let source = DecimalValue::parse("999.999").unwrap();
    invoking.cancel();
    assert!(matches!(
        source.round_to_scale_with_control(2, &control),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(32);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    assert!(matches!(
        source.round_to_scale_with_control(2, &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_truncation_preserves_sign_scale_specials_and_releases_partial_results() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (input, scale, expected) in [
        ("12.999", 0, "12"),
        ("12.999", 2, "12.99"),
        ("-12.999", 2, "-12.99"),
        ("129.99", -1, "120"),
        ("-129.99", -1, "-120"),
        ("0.009", 2, "0.00"),
        ("-0.009", 2, "0.00"),
        ("12", 4, "12.0000"),
        ("NaN", 0, "NaN"),
        ("Infinity", 0, "Infinity"),
    ] {
        let input = DecimalValue::parse(input).unwrap();
        let output = input
            .trunc_to_scale_with_control(scale, &control)
            .unwrap()
            .unwrap();
        assert_eq!(output.to_sql_string(), expected);
        if scale == 0 {
            assert_eq!(output.to_sql_string(), input.trunc().to_sql_string());
        }
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let input = DecimalValue::parse("-123456789.999").unwrap();
    for limit in [0, 16, 32, 64] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            input.trunc_to_scale_with_control(4096, &control),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_absolute_and_directed_integral_rounding_share_finite_and_special_semantics() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (source, absolute, ceiling, floor) in [
        ("-12.000", "12.000", "-12", "-12"),
        ("-12.001", "12.001", "-12", "-13"),
        ("12.001", "12.001", "13", "12"),
        ("99.999", "99.999", "100", "99"),
        ("-99.999", "99.999", "-99", "-100"),
        ("0.0001", "0.0001", "1", "0"),
        ("-0.0001", "0.0001", "0", "-1"),
        ("0.000", "0.000", "0", "0"),
        ("NaN", "NaN", "NaN", "NaN"),
        ("Infinity", "Infinity", "Infinity", "Infinity"),
        ("-Infinity", "Infinity", "-Infinity", "-Infinity"),
    ] {
        let value = DecimalValue::parse(source).unwrap();
        for (output, expected) in [
            (value.abs_with_control(&control).unwrap(), absolute),
            (value.ceil_with_control(&control).unwrap(), ceiling),
            (value.floor_with_control(&control).unwrap(), floor),
        ] {
            assert_eq!(output.to_sql_string(), expected);
        }
        assert_eq!(value.abs().to_sql_string(), absolute);
        assert_eq!(value.ceil().to_sql_string(), ceiling);
        assert_eq!(value.floor().to_sql_string(), floor);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn directed_rounding_and_absolute_value_preserve_quota_and_both_cancellation_errors() {
    let source = DecimalValue::parse("-99999999999999999999.001").unwrap();
    for limit in [0, 1, 16] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for output in [
            source.abs_with_control(&control),
            source.ceil_with_control(&control),
            source.floor_with_control(&control),
        ] {
            assert!(matches!(output, Err(ValueRetentionError::Memory(_))));
            assert_eq!(budget.used(), 0);
        }
    }
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
        for output in [
            source.abs_with_control(&control),
            source.ceil_with_control(&control),
            source.floor_with_control(&control),
        ] {
            assert!(matches!(output, Err(ValueRetentionError::Cancelled(_))));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn normalized_coefficient_reservation_includes_integer_division_slack() {
    // num-bigint's subtraction retains these 7/11/15-limb clones after cancellation down to 1/2/3 limbs: len == capacity / 4 does not trigger shrinking.
    for result_limbs in [1_usize, 2, 3] {
        let capacity = 4 * result_limbs + 3;
        let high = BigInt::from(1_u8) << ((capacity - 1) * usize::BITS as usize);
        let low = BigInt::from(1_u8) << ((result_limbs - 1) * usize::BITS as usize);
        let source = &high + &low;
        let coefficient = source - high;
        assert_eq!(coefficient, low);
        let value = DecimalValue::with_repr(DecimalRepr::Finite {
            coefficient,
            scale: 0,
        });
        let required = size_of::<DecimalRepr>() + capacity * size_of::<usize>();
        assert_eq!(value.retained_bytes(), required);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let budget = MemoryBudget::new(required - 1);
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            value.clone_with_control(&control),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(budget.used(), 0);
        let budget = MemoryBudget::new(required);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let copied = value.clone_with_control(&control).unwrap();
        assert_eq!(*copied, value);
        assert_eq!(copied.reserved_bytes(), required);
        drop(copied);
        assert_eq!(budget.used(), 0);
    }
}
