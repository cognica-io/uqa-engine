//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn prepared_numeric_results_resolve_once_and_preserve_width_budget_and_cancellation() {
    let binding = FunctionBinding {
        object_id: None,
        name: "pg_catalog.abs".into(),
        argument_types: vec!["integer".into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    };
    let site = BuiltinCallMetadata::new(&binding);
    let zero = MemoryBudget::new(0);
    let normal = MemoryBudget::new(16 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let tiny = ProductionControl::new(&zero, &original, &invoking);
    let control = ProductionControl::new(&normal, &original, &invoking);
    assert_eq!(
        BuiltinCallMetadata::numeric_result(Some(&site), &binding, &tiny)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(site.numeric_result.get().is_none());
    assert_eq!(
        BuiltinCallMetadata::numeric_result(Some(&site), &binding, &control).unwrap(),
        Some(NumericWidth::Integer)
    );
    let allocation = allocation_counter::measure(|| {
        for i in 0..10_000 {
            let width = BuiltinCallMetadata::numeric_result(Some(&site), &binding, &tiny)
                .unwrap()
                .unwrap();
            assert_eq!(*width.coerce(&Value::Int(i), &tiny).unwrap(), Value::Int(i));
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(
        NumericWidth::Integer
            .coerce(&Value::Int(i64::from(i32::MAX) + 1), &tiny)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            BuiltinCallMetadata::numeric_result(Some(&site), &binding, &tiny)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    assert_eq!((normal.used(), zero.used()), (0, 0));
}

#[test]
fn set_returning_builtin_metadata_accepts_catalog_qualification_without_allocation() {
    use crate::semantics::sets::static_setness::builtin_call_returns_set;
    let cases = [
        ("aclexplode", true),
        ("PG_CATALOG.GeNeRaTe_SeRiEs", true),
        ("pg_catalog.JSONB_EACH_TEXT", true),
        ("UnNeSt", true),
        ("public.generate_series", false),
        ("PG_CATALOG.pg_catalog.generate_series", false),
        ("PG_CATALOG.GENERATE_SERIES_extra", false),
        ("generate_ſeries", false),
    ];
    let allocations = allocation_counter::measure(|| {
        for _ in 0..10_000 {
            for (name, expected) in cases {
                assert_eq!(builtin_call_returns_set(name, None), expected, "{name}");
            }
        }
    });
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
}
