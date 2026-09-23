//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_float_text_preserves_thresholds_and_special_values() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (value, expected) in [
        (-0.0, "-0"),
        (1e-4, "0.0001"),
        (1e-5, "1e-05"),
        (1e14, "100000000000000"),
        (1e15, "1e+15"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ] {
        let text = format_float_pg_with_control(value, &control).unwrap();
        assert_eq!(&**text, expected);
        assert_eq!(budget.used(), text.capacity());
        drop(text);
        assert_eq!(budget.used(), 0);
    }
    invoking.cancel();
    assert!(matches!(
        format_float_pg_with_control(1.0, &control),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}
