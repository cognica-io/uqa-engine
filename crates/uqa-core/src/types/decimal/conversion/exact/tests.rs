//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};
use proptest::prelude::*;

#[test]
fn exact_binary_conversion_matches_independent_decimal_values() {
    // Python's decimal.Decimal.from_float supplies the finite values; internal keys normalize signed zero.
    for (value, expected) in [
        (0.0, "0"),
        (-0.0, "0"),
        (1.5, "1.5"),
        (-3.75, "-3.75"),
        (
            0.1,
            "0.1000000000000000055511151231257827021181583404541015625",
        ),
        (9_223_372_036_854_774_784_i64 as f64, "9223372036854774784"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (f64::NAN, "NaN"),
    ] {
        assert_eq!(
            DecimalValue::from_f64_exact(value).to_sql_string(),
            expected
        );
    }
    assert_ne!(
        DecimalValue::from_f64_exact(0.1),
        DecimalValue::from_f64_lossy(0.1).unwrap()
    );
}

#[test]
fn controlled_exact_binary_conversion_owns_scratch_and_preserves_failures() {
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(64 * 1024);
    let control = ProductionControl::new(&budget, &token, &token);
    for value in [
        0.0,
        -0.0,
        0.1,
        -0.1,
        f64::from_bits(1),
        -f64::from_bits(1),
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NAN,
    ] {
        let exact = DecimalValue::from_f64_exact_with_control(value, &control).unwrap();
        assert_eq!(*exact, DecimalValue::from_f64_exact(value));
        assert!(budget.used() > 0);
        drop(exact);
        assert_eq!(budget.used(), 0);
    }
    let empty = MemoryBudget::new(0);
    let denied = ProductionControl::new(&empty, &token, &token);
    assert!(matches!(
        DecimalValue::from_f64_exact_with_control(0.1, &denied),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(empty.used(), 0);
    token.cancel();
    assert!(matches!(
        DecimalValue::from_f64_exact_with_control(0.1, &control),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}

proptest! {
    #[test]
    fn exact_binary_conversion_round_trips_finite_bit_patterns(bits: u64) {
        let value = f64::from_bits(bits);
        if value.is_finite() {
            let restored = DecimalValue::from_f64_exact(value).to_f64().unwrap();
            prop_assert_eq!(restored, value);
            if value != 0.0 {
                prop_assert_eq!(restored.to_bits(), bits);
            }
        }
    }
}
