//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Context;
use crate::plan::{retirement::empty_source, JoinExecutionStrategy, SourcePlan};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn partial_plan_retirement_reuses_depth_capacity_and_drops_without_allocating() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let budget = MemoryBudget::new(64 << 20);
            let sibling = budget.reserve(37).unwrap();
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let root = Context::new(&control).unwrap().child(&control).unwrap();
            let mut current = root.clone();
            let mut source = empty_source();
            for _ in 0..8192 {
                current = current.child(&control).unwrap();
                source = SourcePlan::Join {
                    comma: false,
                    left: Box::new(source),
                    right: Box::new(empty_source()),
                    kind: crate::ast::JoinKind::Inner,
                    on: None,
                    using: None,
                    natural: false,
                    alias: None,
                    column_aliases: Vec::new(),
                    lateral: false,
                    strategy: JoinExecutionStrategy::Auto,
                };
            }
            let retained = budget.used();
            let source = root.retain(source);
            let allocation = allocation_counter::measure(|| {
                for _ in 0..2048 {
                    drop(root.child(&control).unwrap().retain(empty_source()));
                }
                assert_eq!(budget.used(), retained);
                drop(source);
            });
            assert_eq!(allocation.count_total, 0, "{allocation:?}");
            assert_eq!(budget.used(), retained);
            drop((root, current));
            assert_eq!(budget.used(), 37);
            drop(sibling);
        })
        .unwrap()
        .join()
        .unwrap();
}
