//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan_cost::{nodes::PlanCostInput, PlanCost};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn fixture<'p>(
    plan: &'p crate::UnifiedPlan,
    parents: &[Option<usize>],
    control: ProductionControl<'_>,
) -> Produced<Vec<PlanCostNode<'p>>> {
    let crate::UnifiedPlan::Query(query) = plan else {
        unreachable!()
    };
    let mut nodes = ProductionVec::new(control);
    for (index, parent) in parents.iter().copied().enumerate() {
        nodes
            .push_copy(PlanCostNode {
                input: PlanCostInput::Query(query),
                parent,
                cost: PlanCost {
                    rows: index as f64,
                    ..PlanCost::default()
                },
            })
            .unwrap();
    }
    nodes.finish().unwrap()
}

fn plan() -> crate::UnifiedPlan {
    crate::UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
}

#[test]
fn deferred_producers_leave_every_display_subtree_contiguous() {
    let plan = plan();
    let control = ProductionControl::uncontrolled();
    let nodes = fixture(
        &plan,
        &[None, Some(0), Some(1), Some(0), Some(1), Some(3)],
        control,
    );
    let ordered = preorder(nodes, control).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|node| (node.cost.rows, node.parent))
            .collect::<Vec<_>>(),
        [
            (0.0, None),
            (1.0, Some(0)),
            (2.0, Some(1)),
            (4.0, Some(1)),
            (3.0, Some(0)),
            (5.0, Some(4)),
        ]
    );
}

#[test]
fn ordinary_trees_keep_their_buffer_without_allocating_ordering_scratch() {
    let plan = plan();
    let memory = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let nodes = fixture(&plan, &[None, Some(0), Some(1), Some(0)], control);
    let pointer = nodes.as_ptr();
    let occupied = memory.reserve(4096 - memory.used()).unwrap();
    let observed = allocation_counter::measure(|| {
        let ordered = preorder(nodes, control).unwrap();
        assert_eq!(ordered.as_ptr(), pointer);
    });
    assert_eq!(observed.count_total, 0);
    drop(occupied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn rejected_ordering_releases_the_original_tree_and_partial_scratch() {
    let plan = plan();
    for cancelled in [None, Some(true), Some(false)] {
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let nodes = fixture(
            &plan,
            &[None, Some(0), Some(1), Some(0), Some(1), Some(3)],
            control,
        );
        let occupied = memory.reserve(4096 - memory.used() - 64).unwrap();
        if let Some(first) = cancelled {
            if first {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let error = preorder(nodes, control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}

proptest::proptest! {
    #[test]
    fn every_parent_keeps_its_original_children_in_display_order(seeds in proptest::collection::vec(proptest::num::usize::ANY, 1..64)) {
        fn expected(parent: Option<usize>, parents: &[Option<usize>], output: &mut Vec<(usize, Option<usize>)>, displayed_parent: Option<usize>) {
            for (index, actual) in parents.iter().enumerate() {
                if *actual == parent {
                    let displayed = output.len();
                    output.push((index, displayed_parent));
                    expected(Some(index), parents, output, Some(displayed));
                }
            }
        }
        let parents = seeds.iter().enumerate().map(|(index, seed)| {
            if index == 0 { None } else { Some(seed % index) }
        }).collect::<Vec<_>>();
        let mut reference = Vec::new();
        expected(None, &parents, &mut reference, None);
        let plan = plan();
        let control = ProductionControl::uncontrolled();
        let ordered = preorder(fixture(&plan, &parents, control), control).unwrap();
        let observed = ordered.iter().map(|node| (node.cost.rows as usize, node.parent)).collect::<Vec<_>>();
        proptest::prop_assert_eq!(observed, reference);
    }
}
