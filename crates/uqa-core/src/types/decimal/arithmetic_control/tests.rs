//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

/// A controlled binary numeric operation.
type Operation = fn(
    &DecimalValue,
    &DecimalValue,
    &ProductionControl<'_>,
) -> Result<Option<Produced<DecimalValue>>, ValueRetentionError>;

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
    // Forty-three digits exceed a word, so every operation runs its arbitrary-precision kernel.
    let left = numeric("1234567890123456789012345678901234567890.001");
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
    // Word operands retain only the representation of their result, so an allowance below it fails and one of exactly that size suffices.
    let word = numeric("123456789012345678901234567890.001");
    for limit in [0, size_of::<DecimalRepr>() - 1] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for output in [
            word.checked_add_with_control(&right, &control),
            word.checked_sub_with_control(&right, &control),
            word.checked_mul_with_control(&right, &control),
        ] {
            assert!(matches!(output, Err(ValueRetentionError::Memory(_))));
            assert_eq!(budget.used(), 0);
        }
    }
    let budget = MemoryBudget::new(size_of::<DecimalRepr>());
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let operations: [(Operation, &str); 3] = [
        (
            DecimalValue::checked_add_with_control,
            "123456789012345678901234567892.251",
        ),
        (
            DecimalValue::checked_sub_with_control,
            "123456789012345678901234567887.751",
        ),
        (
            DecimalValue::checked_mul_with_control,
            "277777775277777777527777777752.50225",
        ),
    ];
    for (operation, expected) in operations {
        let output = operation(&word, &right, &control).unwrap().unwrap();
        assert_eq!(output.to_sql_string(), expected);
        assert_eq!(output.retained_bytes(), size_of::<DecimalRepr>());
        assert_eq!(budget.used(), size_of::<DecimalRepr>());
        drop(output);
        assert_eq!(budget.used(), 0);
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

#[test]
fn the_integer_digit_limit_holds_without_counting_every_coefficient() {
    let budget = MemoryBudget::new(64 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let largest = numeric(&"9".repeat(MAX_INTEGER_DIGITS));
    let sum = |left: &DecimalValue, right: &str| {
        left.checked_add_with_control(&numeric(right), &control)
            .unwrap()
            .map(|value| value.to_canonical_string())
    };
    assert_eq!(
        sum(&largest, "0").map(|text| text.len()),
        Some(MAX_INTEGER_DIGITS)
    );
    assert_eq!(sum(&largest, "1"), None);
    // Fractional digits do not count toward the limit.
    let fractional = numeric(&format!("{}.5", "9".repeat(MAX_INTEGER_DIGITS)));
    assert!(sum(&fractional, "0.4").is_some());
    assert_eq!(sum(&fractional, "0.5"), None);
    // The bit-length bound may only overestimate the digit count.
    for digits in [1_u32, 2, 18, 19, 20, 38, 39, 40, 309, 310] {
        let below = num_bigint::BigInt::from(10_u8).pow(digits) - 1_u8;
        let exact = below.to_string().len() as u128;
        let bound = u128::from(below.bits()) * 30_103 / 100_000 + 1;
        assert!(bound >= exact, "{digits} digits");
        assert!(!integer_digits_may_exceed_limit(&below, 0));
    }
}

fn finite_parts(value: &DecimalValue) -> (BigInt, u32) {
    match value.repr() {
        DecimalRepr::Finite { coefficient, scale } => (coefficient.big().into_owned(), *scale),
        other => panic!("finite numeric expected, got {other:?}"),
    }
}

#[test]
fn machine_word_paths_match_arbitrary_precision_results_and_release_allowance() {
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let coefficients = [
        BigInt::zero(),
        BigInt::from(1),
        BigInt::from(-1),
        BigInt::from(7),
        BigInt::from(-12_345),
        BigInt::from(1_000_000_000_000_000_000_i64),
        BigInt::from(i64::MAX),
        BigInt::from(i64::MIN),
        BigInt::from(i128::from(i64::MAX) + 1),
        BigInt::from(i128::from(i64::MIN) - 1),
    ];
    let scales = [0, 2, 6, 18, 38, 40];
    let values: Vec<DecimalValue> = coefficients
        .iter()
        .flat_map(|coefficient| {
            scales
                .iter()
                .map(|scale| DecimalValue::finite(coefficient.clone(), *scale).unwrap())
        })
        .collect();
    for left in &values {
        for right in &values {
            let (left_coefficient, left_scale) = finite_parts(left);
            let (right_coefficient, right_scale) = finite_parts(right);
            let scale = left_scale.max(right_scale);
            let left_aligned =
                super::super::align_coefficient(&left_coefficient, left_scale, scale);
            let right_aligned =
                super::super::align_coefficient(&right_coefficient, right_scale, scale);

            assert_eq!(
                left.cmp(right),
                left_aligned.cmp(&right_aligned),
                "{left:?} cmp {right:?}"
            );
            assert_eq!(
                left.cmp_with_control(right, &control).unwrap(),
                left_aligned.cmp(&right_aligned),
                "{left:?} controlled cmp {right:?}"
            );

            let sum = left
                .checked_add_with_control(right, &control)
                .unwrap()
                .unwrap();
            assert_eq!(
                finite_parts(&sum),
                (&left_aligned + &right_aligned, scale),
                "{left:?} + {right:?}"
            );
            let difference = left
                .checked_sub_with_control(right, &control)
                .unwrap()
                .unwrap();
            assert_eq!(
                finite_parts(&difference),
                (&left_aligned - &right_aligned, scale),
                "{left:?} - {right:?}"
            );
            let product = left
                .checked_mul_with_control(right, &control)
                .unwrap()
                .unwrap();
            assert_eq!(
                finite_parts(&product),
                (
                    &left_coefficient * &right_coefficient,
                    left_scale + right_scale
                ),
                "{left:?} * {right:?}"
            );
            assert_eq!(
                sum.reserved_bytes(),
                sum.retained_bytes(),
                "{left:?} + {right:?} charge"
            );
            assert_eq!(
                product.reserved_bytes(),
                product.retained_bytes(),
                "{left:?} * {right:?} charge"
            );
        }
    }
    assert_eq!(budget.used(), 0);
}

/// The sign, coefficient and scale of a test spelling `[-]<digits>e-<scale>`, read with arbitrary-precision arithmetic only.
fn spelled_parts(text: &str) -> (BigInt, u32) {
    let (digits, scale) = text.split_once("e-").unwrap();
    (digits.parse().unwrap(), scale.parse().unwrap())
}

fn aligned(coefficient: &BigInt, scale: u32, target: u32) -> BigInt {
    coefficient * BigInt::from(10_u8).pow(target - scale)
}

#[test]
fn word_coefficients_match_arbitrary_precision_results_across_the_word_boundary() {
    let magnitudes = [
        "0",
        "1",
        "123456789",
        "9223372036854775807",
        "9223372036854775808",
        "170141183460469231731687303715884105727",
        "170141183460469231731687303715884105728",
        "99999999999999999999999999999999999999999",
    ];
    let mut spellings = Vec::new();
    for magnitude in magnitudes {
        for sign in ["", "-"] {
            for scale in [0, 2, 5] {
                spellings.push(format!("{sign}{magnitude}e-{scale}"));
            }
        }
    }
    for left in &spellings {
        let (l, ls) = spelled_parts(left);
        let left_value = numeric(left);
        // Formatting and canonical parts agree with the arbitrary-precision coefficient.
        assert!(DecimalValue::parse(&left_value.to_sql_string())
            .unwrap()
            .has_same_representation(&left_value));
        let (mut canonical, mut canonical_scale) = (l.clone(), ls);
        while canonical_scale > 0 && !canonical.is_zero() && (&canonical % 10_u8).is_zero() {
            canonical /= 10_u8;
            canonical_scale -= 1;
        }
        if canonical.is_zero() {
            canonical_scale = 0;
        }
        assert_eq!(
            left_value.canonical_parts(),
            (canonical.to_string(), canonical_scale),
            "{left}"
        );
        for right in &spellings {
            let (r, rs) = spelled_parts(right);
            let right_value = numeric(right);
            let scale = ls.max(rs);
            let (la, ra) = (aligned(&l, ls, scale), aligned(&r, rs, scale));
            assert_eq!(
                left_value.cmp(&right_value),
                la.cmp(&ra),
                "{left} ? {right}"
            );
            let sum = DecimalValue::finite(&la + &ra, scale).unwrap();
            assert!(
                left_value
                    .checked_add(&right_value)
                    .unwrap()
                    .has_same_representation(&sum),
                "{left} + {right}"
            );
            let difference = DecimalValue::finite(&la - &ra, scale).unwrap();
            assert!(
                left_value
                    .checked_sub(&right_value)
                    .unwrap()
                    .has_same_representation(&difference),
                "{left} - {right}"
            );
            let product = DecimalValue::finite(&l * &r, ls + rs).unwrap();
            assert!(
                left_value
                    .checked_mul(&right_value)
                    .unwrap()
                    .has_same_representation(&product),
                "{left} * {right}"
            );
        }
    }
}

#[test]
fn word_results_retain_only_their_representation_and_cross_the_boundary_both_ways() {
    let header = size_of::<DecimalRepr>();
    assert_eq!(DecimalValue::from_i64(i64::MIN).retained_bytes(), header);
    assert_eq!(
        numeric("12345678901234567890.12345678901234567").retained_bytes(),
        header
    );
    let largest_word = numeric("170141183460469231731687303715884105727");
    assert_eq!(largest_word.retained_bytes(), header);
    let beyond = largest_word.checked_add(&numeric("1")).unwrap();
    assert!(beyond.retained_bytes() > header);
    assert_eq!(
        beyond.to_sql_string(),
        "170141183460469231731687303715884105728"
    );
    assert!(beyond
        .checked_sub(&numeric("1"))
        .unwrap()
        .has_same_representation(&largest_word));
    assert!(numeric("-170141183460469231731687303715884105728")
        .negated()
        .has_same_representation(&beyond));
}
