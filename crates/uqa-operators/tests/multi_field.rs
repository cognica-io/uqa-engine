//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Focused implementation of `TestMultiFieldSearchOperator::test_cost_estimate`.

use uqa_core::IndexStats;
use uqa_operators::{MultiFieldSearchOperator, Operator};

#[test]
fn multi_field_search_cost_scales_with_field_count() {
    let op = MultiFieldSearchOperator::new(vec!["title".into(), "body".into()], "test", None);
    let stats = IndexStats::new(100);
    assert_eq!(op.cost_estimate(&stats), 200.0);
}
