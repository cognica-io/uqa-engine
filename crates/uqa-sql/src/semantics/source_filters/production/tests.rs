//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn selected_conjunctions_admit_before_copying_and_preserve_order_and_cleanup() {
    let terms = vec![
        ScalarExpr::qualified_column("v", "id"),
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
    ];
    let filters = QualifierFilters::from([("v".into(), terms)]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let tiny = MemoryBudget::new(32 << 10);
    let control = ProductionControl::new(&tiny, &original, &invoking);
    let failed = allocation_counter::measure(|| {
        assert_eq!(
            qualifier_filter_with_control(Some(&filters), "v", &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(failed.bytes_total < 128 << 10, "{failed:?}");
    assert_eq!(tiny.used(), 0);
    let memory = MemoryBudget::new(32 << 20);
    let control = ProductionControl::new(&memory, &original, &invoking);
    for count in 0..=2 {
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output =
                combine_filter_refs_with_control(filters["v"][..count].iter(), &control).unwrap();
        });
        assert_eq!(allocation.bytes_current, memory.used() as i64);
        let expected = crate::semantics::source_filters::combine_filters(
            filters["v"][..count].iter().cloned(),
        );
        assert_eq!(output.as_deref(), expected.as_ref());
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            qualifier_filter_with_control(Some(&filters), "v", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            qualifier_filter_with_control(None, "v", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        token.reset();
    }
    let output = qualifier_filter_with_control(Some(&filters), "v", &control).unwrap();
    assert!(output.is_some());
    drop(output);
    assert_eq!(memory.used(), 0);
}
