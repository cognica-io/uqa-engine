//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::compiler::types::{parse_regtype_name, parse_regtype_name_with_control};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn retained_type_names_preserve_parser_identity_and_exact_projected_storage() {
    for input in [
        "integer",
        "\"A.Schema\".\"Odd\"\"Type\"[][]",
        "varchar(12) ARRAY",
        "timestamp(3) with time zone",
        "SETOF integer",
        "   ",
    ] {
        let expected = parse_regtype_name(input).unwrap();
        let budget = MemoryBudget::new(8192);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut actual = None;
        let allocated = allocation_counter::measure(|| {
            actual = Some(parse_regtype_name_with_control(input, &control).unwrap());
        });
        let actual = actual.unwrap();
        assert_eq!(actual.as_deref(), expected.as_ref());
        assert_eq!(allocated.bytes_current, budget.used() as i64);
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn retained_type_names_reject_partial_copies_and_both_cancellation_sources() {
    let input = "\"schema_name\".\"type_name\"[]";
    parse_regtype_name(input).unwrap();
    let cancellation = CancellationToken::new();
    for available in 0..(2 * size_of::<String>() + "schema_name".len() + "type_name".len()) {
        let budget = MemoryBudget::new(available);
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        assert_eq!(
            parse_regtype_name_with_control(input, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
    for first in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if first {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(8192);
        let control = ProductionControl::new(&budget, &original, &invoking);
        // Initialize the token's platform mutex before measuring parser output allocations.
        assert!(control.check().is_err());
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                parse_regtype_name_with_control(input, &control),
                Err(crate::SQLError::Cancelled(_))
            ));
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}
