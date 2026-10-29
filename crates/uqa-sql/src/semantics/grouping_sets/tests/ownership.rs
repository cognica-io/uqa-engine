//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn mutable_grouping_selection_retires_discarded_sets_and_analysis_storage() {
    let UnifiedPlan::Query(mut source) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block");
    };
    block.group_distinct = true;
    block.grouping_sets = ["same", "same", "different"]
        .map(|value| vec![ScalarExpr::Literal(Value::Str(value.repeat(2048)))])
        .into();
    source.analyzed_grouping = Some(AnalyzedGroupingSets {
        original_count: 3,
        retained: vec![0, 2],
    });
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut changed = None;
    let allocation = allocation_counter::measure(|| {
        changed = Some(copied.apply_grouping(&control).unwrap());
    });
    let changed = changed.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(changed.reserved_bytes(), budget.used());
    assert!(changed.analyzed_grouping.is_none());
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        panic!("query block");
    };
    assert!(!block.group_distinct);
    assert_eq!(block.grouping_sets.len(), 2);
    for (set, value) in block.grouping_sets.iter().zip(["same", "different"]) {
        assert_eq!(set, &[ScalarExpr::Literal(Value::Str(value.repeat(2048)))]);
    }
    drop(changed);
    assert_eq!(budget.used(), 0);
}
