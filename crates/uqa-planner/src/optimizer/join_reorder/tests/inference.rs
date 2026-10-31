//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, OwnedMap},
    CancellationToken,
};

fn expression(sql: &str) -> ScalarExpr {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT 1 WHERE {sql}"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    block.r#where.unwrap()
}

#[test]
fn inferred_join_decisions_borrow_expression_and_column_payloads() {
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "a".into(),
            column: "x".repeat(4 * 1024 * 1024).into(),
        }),
        rhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "b".into(),
            column: "id".into(),
        }),
    };
    let relations = ["a", "b"].map(|alias| RelationMetadata {
        alias: alias.into(),
        ..Default::default()
    });
    let owners = ColumnOwners::new(&relations);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(
            implied_join_predicates(&expression, &owners, &ProductionControl::uncontrolled())
                .unwrap(),
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    let output = output.unwrap();
    assert_eq!(output.len(), 1);
    let (key, retained) = output.iter().next().unwrap();
    assert!(std::ptr::eq(*retained, &raw const expression));
    let ScalarExpr::Binary { lhs, .. } = &expression else {
        unreachable!()
    };
    let ScalarExpr::QualifiedColumn { column, .. } = lhs.as_ref() else {
        unreachable!()
    };
    assert_eq!(key.0 .1.as_ptr(), column.as_str().as_ptr());
}

#[test]
fn inferred_join_intersections_keep_first_orientation_and_canonical_order() {
    let expression = expression("(b.y = a.x AND a.x = b.y AND a.z = c.w AND a.pruned = c.pruned) OR (c.w = a.z AND a.x = b.y AND b.extra = c.extra)");
    let relations = ["a", "b", "c"].map(|alias| RelationMetadata {
        alias: alias.into(),
        ..Default::default()
    });
    let owners = ColumnOwners::new(&relations);
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let output = implied_join_predicates(
        &expression,
        &owners,
        &ProductionControl::new(&budget, &token, &token),
    )
    .unwrap();
    let observed: Vec<_> = output.iter().map(|(key, value)| (*key, *value)).collect();
    assert_eq!(
        observed.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        [(("a", "x"), ("b", "y")), (("a", "z"), ("c", "w"))]
    );
    let ScalarExpr::Or(branches) = &expression else {
        panic!("disjunction")
    };
    let ScalarExpr::And(first) = &branches[0] else {
        panic!("first conjunction")
    };
    assert!(std::ptr::eq(observed[0].1, &raw const first[0]));
    assert!(std::ptr::eq(observed[1].1, &raw const first[2]));
    let first = join_predicate(observed[0].1, &owners).unwrap();
    assert_eq!(
        (
            first.left_alias,
            first.left_field,
            first.right_alias,
            first.right_field
        ),
        ("b", "y", "a", "x")
    );
    let node =
        OwnedMap::<super::super::inference::JoinPredicateKey<'_>, &ScalarExpr>::entry_bytes();
    assert_eq!(budget.used(), 2 * node);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn inference_workspace_observes_ancestor_limits_cancellation_and_unwind() {
    let input = expression("a.x = b.y AND a.z = c.w");
    let relations = ["a", "b", "c"].map(|alias| RelationMetadata {
        alias: alias.into(),
        ..Default::default()
    });
    let owners = ColumnOwners::new(&relations);
    let node =
        OwnedMap::<super::super::inference::JoinPredicateKey<'_>, &ScalarExpr>::entry_bytes();
    let pruned = expression("(a.x = b.y AND a.z = c.w) OR a.x = b.y");
    for (input, available) in [(&input, 0), (&input, 2 * node), (&pruned, 3 * node)] {
        let parent = MemoryBudget::new(node + available);
        let occupied = parent.reserve(node).unwrap();
        let child = parent.child(8 * node);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&child, &token, &token);
        let allocation = allocation_counter::measure(|| {
            let error = implied_join_predicates(input, &owners, &control)
                .err()
                .expect("admission failure");
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert_eq!(child.used(), 0);
        assert_eq!(parent.used(), node);
        drop(occupied);
        assert_eq!(parent.used(), 0);
    }
    for cancelled in 0..3 {
        let budget = MemoryBudget::new(8 * node);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancelled == 0 {
            original.cancel();
        } else if cancelled == 1 {
            invoking.cancel();
        }
        if cancelled < 2 {
            assert_eq!(
                implied_join_predicates(&input, &owners, &control)
                    .err()
                    .expect("cancellation")
                    .sqlstate(),
                Some("57014")
            );
        } else {
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _output = implied_join_predicates(&input, &owners, &control).unwrap();
                assert_eq!(budget.used(), 2 * node);
                panic!("consumer unwind");
            }));
            assert!(failure.is_err());
        }
        assert_eq!(budget.used(), 0);
    }
}
