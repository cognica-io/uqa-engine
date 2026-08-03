//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Semi-join cost-model coverage.

use uqa_planner::{CostEstimator, OperatorKind};

#[test]
fn semi_and_anti_join_costs_include_both_inputs() {
    let estimator = CostEstimator::default();
    let semi = estimator.estimate_join(OperatorKind::SemiJoin, 100.0, 100.0);
    let anti = estimator.estimate_join(OperatorKind::AntiJoin, 100.0, 100.0);
    assert_eq!(semi.total(), anti.total());
    assert!(semi.total() > 100.0);
}
