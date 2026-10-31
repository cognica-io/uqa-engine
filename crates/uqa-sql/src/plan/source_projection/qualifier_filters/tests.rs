//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn qualifier_collections_admit_names_slots_and_payloads_and_retire_exact_storage() {
    let input = QualifierFilters::from([(
        "v".into(),
        vec![
            ScalarExpr::Column("id".into()),
            ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
        ],
    )]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let tiny = MemoryBudget::new(32 << 10);
    let control = ProductionControl::new(&tiny, &original, &invoking);
    for failure in 0..3 {
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                input.copy_with_control(&control).unwrap_err().sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
        });
        assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
        assert_eq!(tiny.used(), 0);
        original.reset();
        invoking.reset();
    }
    let budget = MemoryBudget::new(32 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(input.copy_with_control(&control).unwrap());
    });
    assert_eq!(output.as_ref(), Some(&input));
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    assert!(budget.used() > 4 << 20);
    drop(output);
    assert_eq!(budget.used(), 0);
}
