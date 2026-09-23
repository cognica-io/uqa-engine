//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

fn numeric(text: &str) -> DecimalValue {
    DecimalValue::parse(text).unwrap()
}

#[test]
fn controlled_arithmetic_preserves_scale_alignment_signed_results_and_specials() {
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (operator, left, right, expected) in [
        (
            '+',
            "99999999999999999999.99",
            "0.01",
            "100000000000000000000.00",
        ),
        ('+', "-12.01", "12.010", "0.000"),
        ('-', "0.001", "12", "-11.999"),
        ('*', "-12.50", "0.20", "-2.5000"),
        ('/', "1", "3", "0.33333333333333333333"),
        ('%', "-12.50", "0.30", "-0.20"),
        ('+', "Infinity", "-Infinity", "NaN"),
        ('*', "0", "Infinity", "NaN"),
        ('/', "-1", "Infinity", "0"),
        ('%', "12.50", "Infinity", "12.50"),
    ] {
        let left = numeric(left);
        let right = numeric(right);
        let output = match operator {
            '+' => left.checked_add_with_control(&right, &control),
            '-' => left.checked_sub_with_control(&right, &control),
            '*' => left.checked_mul_with_control(&right, &control),
            '/' => left.checked_div_postgres_with_control(&right, &control),
            '%' => left.checked_rem_with_control(&right, &control),
            _ => unreachable!(),
        }
        .unwrap()
        .unwrap();
        assert_eq!(output.to_sql_string(), expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert!(numeric("1")
        .checked_div_postgres_with_control(&numeric("0"), &control)
        .unwrap()
        .is_none());
    assert!(numeric("1")
        .checked_rem_with_control(&numeric("0"), &control)
        .unwrap()
        .is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_power_keeps_postgresql_integer_fractional_and_negative_scale_selection() {
    let budget = MemoryBudget::new(64 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (base, exponent, expected) in [
        ("2", "0.5", "1.4142135623730950"),
        ("4", "0.25", "1.4142135623730950"),
        ("2", "3", "8.0000000000000000"),
        ("2", "-3", "0.1250000000000000"),
        ("-2", "3", "-8.0000000000000000"),
        ("NaN", "0", "1"),
        ("1", "NaN", "1"),
        ("-Infinity", "3", "-Infinity"),
    ] {
        let base = numeric(base);
        let exponent = numeric(exponent);
        let output = base
            .checked_pow_postgres_with_control(&exponent, &control)
            .unwrap()
            .unwrap();
        assert_eq!(output.to_sql_string(), expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert!(numeric("-2")
        .checked_pow_postgres_with_control(&numeric("0.5"), &control)
        .unwrap()
        .is_none());
    assert!(numeric("0")
        .checked_pow_postgres_with_control(&numeric("-1"), &control)
        .unwrap()
        .is_none());
    for (text, expected) in [
        ("0.000", true),
        ("12.000", true),
        ("-12.001", false),
        ("0.0001", false),
        ("Infinity", true),
        ("NaN", false),
    ] {
        assert_eq!(
            numeric(text).is_integral_with_control(&control).unwrap(),
            expected
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn arithmetic_quota_and_cancellation_release_all_native_and_power_intermediates() {
    let left = numeric("123456789012345678901234567890.001");
    let right = numeric("2.25");
    for limit in [0, 1, 64, 128] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for output in [
            left.checked_add_with_control(&right, &control),
            left.checked_sub_with_control(&right, &control),
            left.checked_mul_with_control(&right, &control),
            left.checked_div_postgres_with_control(&right, &control),
            left.checked_rem_with_control(&right, &control),
            left.checked_pow_postgres_with_control(&right, &control),
        ] {
            assert!(matches!(output, Err(ValueRetentionError::Memory(_))));
            assert_eq!(budget.used(), 0);
        }
    }
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for output in [
            left.checked_add_with_control(&right, &control),
            left.checked_sub_with_control(&right, &control),
            left.checked_mul_with_control(&right, &control),
            left.checked_div_postgres_with_control(&right, &control),
            left.checked_rem_with_control(&right, &control),
            left.checked_pow_postgres_with_control(&right, &control),
        ] {
            assert!(matches!(output, Err(ValueRetentionError::Cancelled(_))));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn multiplication_and_division_preserve_fractional_scale_limits_and_midpoint_rounding() {
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let smallest = numeric("1e-16383");
    let product = smallest
        .checked_mul_with_control(&smallest, &control)
        .unwrap()
        .unwrap();
    assert!(product.is_zero());
    assert_eq!(product.display_scale(), Some(MAX_FRACTIONAL_DIGITS));
    assert_eq!(budget.used(), product.reserved_bytes());
    drop(product);
    for (left, right, scale, expected) in [
        ("2", "3", 20, "0.66666666666666666667"),
        ("-1", "8", 2, "-0.13"),
        ("1", "8", 2, "0.13"),
    ] {
        let value = numeric(left)
            .checked_div_to_scale_with_control(&numeric(right), scale, &control)
            .unwrap()
            .unwrap();
        assert_eq!(value.to_sql_string(), expected);
        assert_eq!(budget.used(), value.reserved_bytes());
        drop(value);
    }
    assert!(numeric("1")
        .checked_div_to_scale_with_control(&numeric("3"), MAX_FRACTIONAL_DIGITS + 1, &control)
        .unwrap()
        .is_none());
    assert_eq!(budget.used(), 0);
}
