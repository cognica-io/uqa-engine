//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, ValueRetentionError,
};

#[test]
fn controlled_comparison_matches_existing_order_without_retaining_scratch() {
    let values = [
        "-Infinity",
        "-1000",
        "-0.1001",
        "-0.1",
        "-0.00",
        "0",
        "0.00001",
        "0.1",
        "0.1000",
        "1.00",
        "1e1000",
        "Infinity",
        "NaN",
    ]
    .map(|text| DecimalValue::parse(text).unwrap());
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for left in &values {
        for right in &values {
            assert_eq!(
                left.cmp_with_control(right, &control).unwrap(),
                left.cmp(right)
            );
            assert_eq!(budget.used(), 0);
        }
    }
    original.cancel();
    assert!(matches!(
        values[0].cmp_with_control(&values[1], &control),
        Err(ValueRetentionError::Cancelled(_))
    ));
}
