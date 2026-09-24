//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn routine_names_preserve_alias_array_quote_and_modifier_normalization() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, expected) in [
        ("PG_CATALOG.INTEGER", "int4"),
        (" TIMESTAMP ( 3 )  WITH TIME ZONE [] ", "timestamptz[]"),
        ("numeric(10, -2)[][]", "numeric[][]"),
        ("character   varying (5)", "varchar"),
        ("\"Custom (type)\"", "\"custom (type)\""),
        ("\"a\"\"(b)\"(4)", "\"a\"\"(b)\""),
        ("x(one(two)) y", "x y"),
        ("x(1)y", "xy"),
    ] {
        let result = canonical_routine_type_name_with_control(source, &control).unwrap();
        assert_eq!(&*result, expected);
        assert_eq!(canonical_routine_type_name(source), expected);
        assert_eq!(result.reserved_bytes(), result.capacity());
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn routine_name_failure_releases_partial_scratch_and_keeps_cancellation() {
    let budget = MemoryBudget::new(16);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(canonical_routine_type_name_with_control(
        "custom_type_name_that_needs_more_than_one_buffer",
        &control
    )
    .is_err());
    assert!(budget.peak() > 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            canonical_routine_type_name_with_control("", &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}
