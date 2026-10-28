//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, MemoryError},
    CancellationToken,
};

#[test]
fn selected_join_tree_moves_owned_statistics_without_copying_payloads() {
    let mut left = rel_with_stats("a", 100.0, 1, "id", 100);
    let payload = "x".repeat(4 * 1024 * 1024);
    let original = payload.as_ptr();
    left.column_stats.get_mut("id").unwrap().min_value = Some(Value::Str(payload));
    let relations = vec![left, rel_with_stats("b", 100.0, 2, "id", 100)];
    let predicates = vec![JoinPredicate {
        left_alias: "a".into(),
        right_alias: "b".into(),
        left_field: "id".into(),
        right_field: "id".into(),
    }];
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(
            JoinOrderOptimizer::new()
                .optimize(relations, predicates)
                .unwrap(),
        );
    });
    assert!(allocation.bytes_total < 64 * 1024, "{allocation:?}");
    let result = result.unwrap();
    let relation = find_source(&result.tree, 1).unwrap();
    let Some(Value::Str(value)) = &relation.column_stats["id"].min_value else {
        panic!("selected relation lost its statistics");
    };
    assert_eq!(value.as_ptr(), original);
    assert_eq!(value.len(), 4 * 1024 * 1024);
    assert_eq!(result.primary_alias.as_deref(), Some("a"));
}

fn find_source(tree: &JoinOrderTree, source_id: u64) -> Option<&JoinRelation> {
    match tree {
        JoinOrderTree::Scan(relation) => (relation.source_id == source_id).then_some(relation),
        JoinOrderTree::Inner { left, right, .. } | JoinOrderTree::Cross { left, right } => {
            find_source(left, source_id).or_else(|| find_source(right, source_id))
        }
    }
}

fn fixture(count: usize, dense: bool) -> (Vec<JoinRelation>, Vec<JoinPredicate>, JoinGraph) {
    let mut graph = JoinGraph::new();
    let relations: Vec<_> = (0..count)
        .map(|index| {
            let relation = rel_with_stats(
                &format!("relation_{}", 64 - index),
                10.0 + index as f64,
                index as u64,
                "id",
                100,
            );
            graph
                .add_relation_with_cost(&relation.alias, relation.cardinality, relation.access_cost)
                .unwrap();
            relation
        })
        .collect();
    let mut predicates = Vec::new();
    for right in 1..count {
        for left in 0..right {
            if dense || (left + 1 == right && right % 5 != 0) {
                predicates.push(JoinPredicate {
                    left_alias: relations[left].alias.clone(),
                    right_alias: relations[right].alias.clone(),
                    left_field: "id".into(),
                    right_field: "id".into(),
                });
                graph.add_edge(left, right, 0.01).unwrap();
            }
        }
    }
    (relations, predicates, graph)
}

fn same_plan(actual: &JoinPlan, expected: &JoinPlan) {
    assert_eq!(actual.relations, expected.relations);
    assert_eq!(actual.cardinality.to_bits(), expected.cardinality.to_bits());
    assert_eq!(actual.cost.to_bits(), expected.cost.to_bits());
    assert_eq!(actual.kind, expected.kind);
    let edge = |edge: Option<crate::JoinEdge>| {
        edge.map(|edge| (edge.left, edge.right, edge.selectivity.to_bits()))
    };
    assert_eq!(edge(actual.join_edge), edge(expected.join_edge));
    for (actual, expected) in [
        (&actual.left, &expected.left),
        (&actual.right, &expected.right),
    ] {
        match (actual, expected) {
            (Some(actual), Some(expected)) => same_plan(actual, expected),
            (None, None) => {}
            _ => panic!("selection changed the join tree"),
        }
    }
}

#[test]
fn join_selection_admits_graph_storage_and_releases_it_before_return() {
    for (count, dense) in [(1, false), (2, true), (8, true), (18, false), (64, false)] {
        let (relations, predicates, graph) = fixture(count, dense);
        let expected = crate::enumerate_dpccp(&graph).unwrap();
        let optimizer = JoinOrderOptimizer::new();
        let budget = MemoryBudget::new(16 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(
                optimizer
                    .select_with_control(&relations, &predicates, &control)
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        same_plan(&output, &expected);
        let retained = 2 * (count - 1) * size_of::<JoinPlan>();
        assert_eq!(output.reserved_bytes(), retained);
        assert_eq!(budget.used(), retained);
        assert_eq!(allocation.bytes_current as usize, retained);
        assert!(
            allocation.bytes_max as usize <= budget.peak(),
            "{allocation:?}"
        );
        let required = budget.peak();
        drop(output);
        assert_eq!(budget.used(), 0);

        for limit in [0, 1, required / 2, required - 1, required] {
            let budget = MemoryBudget::new(limit);
            let control = ProductionControl::new(&budget, &token, &token);
            let result = optimizer.select_with_control(&relations, &predicates, &control);
            if limit < required {
                assert!(
                    matches!(
                        result,
                        Err(JoinOrderError::Production(ValueRetentionError::Memory(
                            MemoryError::Limit { .. }
                        )))
                    ),
                    "count={count}, limit={limit}: {result:?}"
                );
            } else {
                same_plan(result.as_ref().unwrap(), &expected);
            }
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn rejected_or_cancelled_join_selection_does_not_copy_graph_inputs() {
    let relations = vec![
        rel(&"a".repeat(4 * 1024 * 1024), 10.0, 1),
        rel("b", 20.0, 2),
    ];
    for cancelled in 0..3 {
        let budget = MemoryBudget::new(if cancelled == 0 { 0 } else { usize::MAX });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancelled == 1 {
            original.cancel();
        } else if cancelled == 2 {
            invoking.cancel();
        }
        // Initialize the standard library's thread identity outside the count.
        let _ = original.check();
        let _ = invoking.check();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let allocation = allocation_counter::measure(|| {
            let error = JoinOrderOptimizer::new()
                .select_with_control(&relations, &[], &control)
                .unwrap_err();
            if cancelled == 0 {
                assert!(matches!(
                    error,
                    JoinOrderError::Production(ValueRetentionError::Memory(
                        MemoryError::Limit { .. }
                    ))
                ));
            } else {
                assert!(matches!(
                    error,
                    JoinOrderError::Production(ValueRetentionError::Cancelled(_))
                ));
            }
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn join_selection_preserves_input_errors_and_releases_partial_graphs() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let optimizer = JoinOrderOptimizer::new();
    let (mut many, _, _) = fixture(64, false);
    many.push(rel("overflow", f64::NAN, 65));
    let unknown = JoinPredicate {
        left_alias: "unknown_left".into(),
        right_alias: "unknown_right".into(),
        left_field: "id".into(),
        right_field: "id".into(),
    };
    for (relations, predicates, expected) in [
        (vec![], vec![], JoinGraphError::EmptyGraph),
        (
            vec![rel("a", 10.0, 1), rel("a", -1.0, 2)],
            vec![],
            JoinGraphError::InvalidCardinality {
                name: "a".into(),
                rows: -1.0,
            },
        ),
        (
            vec![rel("a", 10.0, 1), rel("a", 20.0, 2)],
            vec![],
            JoinGraphError::DuplicateAlias { alias: "a".into() },
        ),
        (
            many,
            vec![],
            JoinGraphError::TooManyRelations {
                name: "overflow".into(),
                index: 64,
            },
        ),
        (
            vec![rel("a", 10.0, 1), rel("b", 20.0, 2)],
            vec![unknown.clone()],
            JoinGraphError::UnknownAlias {
                alias: "unknown_left".into(),
            },
        ),
    ] {
        let allocation = allocation_counter::measure(|| {
            let error = optimizer
                .select_with_control(&relations, &predicates, &control)
                .unwrap_err();
            let JoinOrderError::Graph(error) = error else {
                panic!("changed graph diagnostic: {error}");
            };
            assert_eq!(error, expected);
        });
        assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
        assert_eq!(budget.used(), 0);
    }
    let output = optimizer
        .select_with_control(&[rel("only", 10.0, 1)], &[unknown], &control)
        .unwrap();
    assert_eq!(output.relations, 1);
    assert_eq!(output.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn join_selection_respects_ancestor_admission_and_consumer_unwind() {
    let (relations, predicates, _) = fixture(8, true);
    let budget = MemoryBudget::new(1024 * 1024);
    let child = budget.child(1024 * 1024);
    let sibling = budget.reserve(budget.limit()).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&child, &token, &token);
    let optimizer = JoinOrderOptimizer::new();
    assert!(optimizer
        .select_with_control(&relations, &predicates, &control)
        .is_err());
    assert_eq!(child.used(), 0);
    assert_eq!(budget.used(), budget.limit());
    drop(sibling);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _output = optimizer
            .select_with_control(&relations, &predicates, &control)
            .unwrap();
        assert!(budget.used() > 0);
        panic!("consumer unwound with its selected plan");
    }));
    assert!(unwind.is_err());
    assert_eq!(child.used(), 0);
    assert_eq!(budget.used(), 0);
    let output = optimizer
        .select_with_control(&relations, &predicates, &control)
        .unwrap();
    drop(output);
    assert_eq!(budget.used(), 0);
}
