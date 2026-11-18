//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{
    mutable::{MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan},
    retirement::{empty_query, empty_source},
    CommandPlan,
};

fn assert_read(bytes: usize, read: impl Fn() -> usize) {
    let allocation = allocation_counter::measure(|| assert_eq!(read(), bytes));
    assert_eq!(allocation.count_total, 0);
}

#[test]
fn deep_plan_field_footprints_preserve_subtree_ownership_with_exhausted_allowance() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let independent = native.reserve(47).unwrap();
            let mut fields = BudgetedVec::new(&native);
            fields.reserve(2).unwrap();
            let before_left = native.used();
            let left = chain(8192, &native).unwrap();
            let left_bytes = native.used() - before_left;
            let before_right = native.used();
            let right = chain(4096, &native).unwrap();
            let right_bytes = native.used() - before_right;
            fields
                .push(RewriteField {
                    name: "left",
                    memory: Some(left),
                })
                .unwrap();
            fields
                .push(RewriteField {
                    name: "right",
                    memory: Some(right),
                })
                .unwrap();
            let owner = RewriteMemory::fields(native.reserve(19).unwrap(), fields);
            let total = native.used() - independent.bytes();
            let exhausted = native.reserve(native.available()).unwrap();
            let mut query = MutableQueryPlan::from_rewrite_parts(empty_query(), Some(owner));
            assert_read(total, || query.reserved_bytes());

            let root = query.memory.as_mut().unwrap();
            let child = root.field_mut("left").take();
            let source = MutableSourcePlan {
                source: empty_source(),
                memory: child.into(),
            };
            assert_read(left_bytes, || source.reserved_bytes());
            assert_read(total - left_bytes, || query.reserved_bytes());

            let root = query.memory.as_mut().unwrap();
            let right = std::mem::replace(root.field_mut("right"), source.memory.into_parts());
            let command = MutableCommandPlan {
                command: CommandPlan::Empty,
                memory: right.into(),
            };
            assert_read(right_bytes, || command.reserved_bytes());
            assert_read(total - right_bytes, || query.reserved_bytes());
            let root = query.memory.as_mut().unwrap();
            *root.item_mut(0) = command.memory.into_parts();
            let executable = MutableExecutablePlan::Query(query);
            assert_read(total, || executable.reserved_bytes());
            assert_eq!(native.used(), native.limit());
            drop(executable);
            assert_eq!(native.used(), exhausted.bytes() + independent.bytes());
            drop((exhausted, independent));
            assert_eq!(native.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn concurrent_plan_footprint_readers_share_the_original_field_records() {
    let native = MemoryBudget::new(32 << 20);
    let mut owner = chain(4096, &native).unwrap();
    let RewriteMemory::Fields { fields, .. } = &mut owner else {
        unreachable!()
    };
    let address = fields.as_ptr();
    let expected = native.used();
    let exhausted = native.reserve(native.limit() - expected).unwrap();
    let mut query = MutableQueryPlan::from_rewrite_parts(empty_query(), Some(owner));
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                start.wait();
                for _ in 0..8 {
                    assert_read(expected, || query.reserved_bytes());
                }
            });
        }
    });
    let Some(RewriteMemory::Fields { fields, .. }) = query.memory.as_mut() else {
        unreachable!()
    };
    assert_eq!(fields.as_ptr(), address);
    drop(query);
    assert_eq!(native.used(), exhausted.bytes());
}

#[test]
fn nested_query_construction_and_extraction_keep_exact_public_footprints() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let token = uqa_core::CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let sibling = native.reserve(41).unwrap();
            let crate::plan::UnifiedPlan::Query(leaf) =
                crate::plan::UnifiedPlan::lower(crate::compile("SELECT 7").unwrap().remove(0))
            else {
                unreachable!()
            };
            let mut query = leaf.copy_for_rewrite(&control).unwrap();
            for _ in 0..1024 {
                let columns = control
                    .finish(Vec::new(), control.empty_reservation())
                    .unwrap();
                let source =
                    MutableSourcePlan::subquery(query, None, columns, None, false, &control)
                        .unwrap();
                query = MutableQueryPlan::projected(std::iter::empty(), &control)
                    .unwrap()
                    .replace_source(Some(source), &control)
                    .unwrap();
            }
            let exhausted = native.reserve(native.available()).unwrap();
            let independent = exhausted.bytes() + sibling.bytes();
            for _ in 0..1024 {
                assert_read(native.used() - independent, || query.reserved_bytes());
                let (shell, source) = query.take_source(&control).unwrap();
                drop(shell);
                let source = source.unwrap();
                assert_read(native.used() - independent, || source.reserved_bytes());
                query = source.into_query(&control).unwrap();
            }
            let published = query.finish(&control).unwrap();
            assert_eq!(
                serde_json::to_value(&*published).unwrap(),
                serde_json::to_value(&*leaf).unwrap()
            );
            assert_eq!(published.reserved_bytes(), native.used() - independent);
            drop(published);
            assert_eq!(native.used(), independent);
        })
        .unwrap()
        .join()
        .unwrap();
}
