//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan_cost::PlanCostEstimator, RelationStats, UnifiedPlan};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
}

#[test]
fn capture_uses_the_root_traversal_and_keeps_child_invocation_costs() {
    let calls = Cell::new(0);
    let statistics = |_: &str| {
        calls.set(calls.get() + 1);
        Some(RelationStats {
            row_count: 100,
            ..Default::default()
        })
    };
    let estimator = PlanCostEstimator::new(&statistics);
    let control = ProductionControl::uncontrolled();
    for sql in [
        "SELECT id FROM items LIMIT 3",
        "SELECT a.id FROM items a JOIN items b ON a.id=b.id",
        "SELECT id FROM items UNION ALL SELECT id FROM items",
        "SELECT (SELECT count(*) FROM items) FROM items LIMIT 5",
        "WITH x AS MATERIALIZED (SELECT id FROM items) SELECT * FROM x",
        "INSERT INTO items SELECT id FROM items",
        "UPDATE items SET id=2 FROM other WHERE items.id=other.id",
        "DELETE FROM items USING other WHERE items.id=other.id",
    ] {
        let body = plan(sql);
        calls.set(0);
        let expected = estimator.estimate_with_control(&body, &control).unwrap();
        let root_calls = calls.replace(0);
        let tree = estimator
            .estimate_nodes_with_control(&body, &control)
            .unwrap();
        assert_eq!(tree.root, expected, "{sql}");
        assert_eq!(tree.nodes[0].cost, expected, "{sql}");
        assert_eq!(
            calls.get(),
            root_calls,
            "child capture repeated statistics: {sql}"
        );
        assert_eq!(tree.nodes[0].parent, None);
        for (index, node) in tree.nodes.iter().enumerate().skip(1) {
            assert!(
                node.parent.is_some_and(|parent| parent < index),
                "{sql}: {node:?}"
            );
        }
        if sql.starts_with("SELECT (SELECT") {
            let child = tree
                .nodes
                .iter()
                .skip(1)
                .find(|node| matches!(node.input, PlanCostInput::Query(_)))
                .unwrap();
            let independent = estimator.estimate(&plan("SELECT count(*) FROM items"));
            assert_eq!(child.cost, independent);
            assert!(tree.root.execution > child.cost.execution);
        }
    }
}

#[test]
fn child_estimates_keep_lexical_cte_shadowing_and_original_input_borrows() {
    let body = plan(
        "WITH x AS (VALUES (1),(2),(3)) SELECT * FROM (WITH x AS (VALUES (1),(2)) SELECT * FROM x) local_x CROSS JOIN x",
    );
    let statistics = |name: &str| -> Option<RelationStats> {
        panic!("lexical CTE must not read table statistics: {name}")
    };
    let tree = PlanCostEstimator::new(&statistics)
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(tree.root.rows, 6.0);
    let UnifiedPlan::Query(root) = &body else {
        unreachable!()
    };
    let PlanCostInput::Query(input) = tree.nodes[0].input else {
        unreachable!()
    };
    assert!(std::ptr::eq(input, root.as_ref()));
    let rows = tree.nodes.iter().filter_map(|node| {
        matches!(node.input, PlanCostInput::Source(SourcePlan::Table { name, .. }) if name == "x")
            .then_some(node.cost.rows)
    }).collect::<Vec<_>>();
    assert_eq!(rows, [2.0, 3.0]);
}

#[test]
fn captured_nodes_follow_retained_named_inputs_without_fallback_statistics() {
    let mut body = plan("SELECT * FROM selected_view");
    let UnifiedPlan::Query(query) = &mut body else {
        unreachable!()
    };
    let crate::RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let SourcePlan::Table {
        executable_query, ..
    } = block.from.as_mut().unwrap()
    else {
        unreachable!()
    };
    let UnifiedPlan::Query(selected) = plan("VALUES (1),(2),(3)") else {
        unreachable!()
    };
    *executable_query = Some(selected);
    let statistics = |name: &str| -> Option<RelationStats> {
        panic!("selected view must not read table statistics: {name}")
    };
    let tree = PlanCostEstimator::new(&statistics)
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(tree.nodes.len(), 3);
    assert_eq!(tree.nodes[1].parent, Some(0));
    assert_eq!(tree.nodes[2].parent, Some(1));
    assert!(tree.nodes.iter().all(|node| node.cost.rows == 3.0));
}

#[test]
fn capture_admits_before_statistics_and_releases_every_failed_partial_tree() {
    let body = plan("SELECT * FROM a JOIN b ON a.id=b.id JOIN c ON c.id=b.id");
    let calls = Cell::new(0);
    let statistics = |_: &str| {
        calls.set(calls.get() + 1);
        Some(RelationStats::default())
    };
    let estimator = PlanCostEstimator::new(&statistics);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for allowance in [0, 64, 256] {
        let memory = MemoryBudget::new(allowance);
        let control = ProductionControl::new(&memory, &original, &invoking);
        calls.set(0);
        let error = estimator
            .estimate_nodes_with_control(&body, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
        if allowance == 0 {
            assert_eq!(calls.get(), 0);
        }
    }
    let memory = MemoryBudget::new(16 * 1024);
    let control = ProductionControl::new(&memory, &original, &invoking);
    let tree = estimator
        .estimate_nodes_with_control(&body, &control)
        .unwrap();
    assert!(memory.used() >= tree.nodes.len() * std::mem::size_of::<PlanCostNode<'_>>());
    drop(tree);
    assert_eq!(memory.used(), 0);
}

#[test]
fn cancellation_and_statistics_unwind_cannot_publish_a_partial_cost_tree() {
    for sql in [
        "SELECT * FROM a JOIN b ON a.id=b.id",
        "WITH x AS MATERIALIZED (SELECT * FROM a JOIN b ON a.id=b.id) SELECT * FROM x",
    ] {
        let body = plan(sql);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = MemoryBudget::new(16 * 1024);
        let control = ProductionControl::new(&memory, &original, &invoking);
        for token in [&original, &invoking] {
            let statistics = |_: &str| {
                token.cancel();
                Some(RelationStats::default())
            };
            let error = PlanCostEstimator::new(&statistics)
                .estimate_nodes_with_control(&body, &control)
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        let fail = Cell::new(true);
        let statistics = |_: &str| -> Option<RelationStats> {
            assert!(!fail.get(), "statistics failure");
            Some(RelationStats::default())
        };
        let estimator = PlanCostEstimator::new(&statistics);
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            estimator.estimate_nodes_with_control(&body, &control)
        }));
        assert!(failure.is_err());
        assert_eq!(memory.used(), 0);
        fail.set(false);
        let recovered = estimator
            .estimate_nodes_with_control(&body, &control)
            .unwrap();
        assert!(!recovered.nodes.is_empty());
        drop(recovered);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn shared_cte_startup_belongs_to_the_producer_instead_of_each_reference() {
    let body = plan(
        "WITH x AS MATERIALIZED (SELECT id FROM items ORDER BY id) SELECT a.id FROM x a CROSS JOIN x b",
    );
    let statistics = |_: &str| {
        Some(RelationStats {
            row_count: 3,
            ..Default::default()
        })
    };
    let estimator = PlanCostEstimator::new(&statistics);
    let tree = estimator
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    let producer = tree
        .nodes
        .iter()
        .skip(1)
        .find(|node| matches!(node.input, PlanCostInput::Query(_)))
        .unwrap();
    assert!(producer.cost.startup > 0.0);
    assert_eq!(tree.root.startup, producer.cost.startup);
    assert_eq!(tree.root.rows, 9.0);
    let references = tree.nodes.iter().filter(|node| matches!(node.input, PlanCostInput::Source(SourcePlan::Table { name, .. }) if name == "x")).collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    for node in references {
        assert_eq!(node.cost.startup, 0.0);
        assert_eq!(node.cost.rows, 3.0);
    }
    for node in tree.nodes.iter() {
        assert!(node.cost.startup <= node.cost.execution, "{node:?}");
    }
}

#[test]
fn cte_estimates_decode_names_and_follow_reachable_dependency_scopes() {
    let statistics = |name: &str| -> Option<RelationStats> {
        panic!("reachable CTE must not read relation statistics: {name}")
    };
    let estimator = PlanCostEstimator::new(&statistics);
    for (sql, rows) in [
        (
            "WITH \"Quoted Name\" AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM \"Quoted Name\"",
            2.0,
        ),
        (
            "WITH \"quoted\"\"name\" AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM \"quoted\"\"name\"",
            2.0,
        ),
        (
            "WITH RECURSIVE a AS MATERIALIZED (SELECT * FROM b),b AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM a",
            2.0,
        ),
        (
            "WITH x AS MATERIALIZED (VALUES(1),(2),(3)) SELECT * FROM (WITH a AS MATERIALIZED (SELECT * FROM x),x AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM x CROSS JOIN a) s",
            6.0,
        ),
    ] {
        let body = plan(sql);
        let tree = estimator
            .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
            .unwrap();
        assert_eq!(tree.root.rows, rows, "{sql}");
    }
    let body = plan("WITH unused AS MATERIALIZED (SELECT * FROM unvisited) SELECT 7");
    let tree = estimator
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(tree.nodes.len(), 1);
    assert_eq!(tree.root, estimator.estimate(&plan("SELECT 7")));
}

#[test]
fn retained_catalog_views_do_not_capture_the_callers_cte_namespace() {
    let mut body =
        plan("WITH x AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM selected_view CROSS JOIN x");
    let UnifiedPlan::Query(query) = &mut body else {
        unreachable!()
    };
    let crate::RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let SourcePlan::Join { left, .. } = block.from.as_mut().unwrap() else {
        unreachable!()
    };
    let SourcePlan::Table {
        executable_query, ..
    } = left.as_mut()
    else {
        unreachable!()
    };
    let UnifiedPlan::Query(selected) = plan("SELECT * FROM x") else {
        unreachable!()
    };
    *executable_query = Some(selected);
    let calls = Cell::new(0);
    let statistics = |name: &str| {
        assert_eq!(name, "x");
        calls.set(calls.get() + 1);
        Some(RelationStats {
            row_count: 3,
            ..Default::default()
        })
    };
    let tree = PlanCostEstimator::new(&statistics)
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(tree.root.rows, 6.0);
    assert_eq!(calls.get(), 1);
}

fn retain_named_input(source: &mut SourcePlan, target: &str, query: &crate::QueryPlan) {
    match source {
        SourcePlan::Table {
            name,
            executable_query,
            ..
        } if name == target => {
            *executable_query = Some(Box::new(query.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            retain_named_input(left, target, query);
            retain_named_input(right, target, query);
        }
        _ => {}
    }
}

#[test]
fn folded_sources_capture_only_the_selected_query_without_charging_an_original_producer() {
    let mut body = plan("WITH x AS NOT MATERIALIZED (SELECT * FROM unvisited) SELECT * FROM x");
    let UnifiedPlan::Query(selected) = plan("VALUES(1),(2),(3)") else {
        unreachable!()
    };
    let UnifiedPlan::Query(query) = &mut body else {
        unreachable!()
    };
    let crate::RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    retain_named_input(block.from.as_mut().unwrap(), "x", &selected);
    let SourcePlan::Table {
        executable_query: Some(selected),
        ..
    } = block.from.as_ref().unwrap()
    else {
        unreachable!()
    };
    let selected_pointer = std::ptr::from_ref(selected.as_ref());
    let original_pointer = std::ptr::from_ref(query.ctes[0].body.query().unwrap());
    let statistics =
        |name: &str| -> Option<RelationStats> { panic!("unselected producer statistics: {name}") };
    let estimator = PlanCostEstimator::new(&statistics);
    let tree = estimator
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    let expanded = estimator.estimate(&plan("SELECT * FROM (VALUES(1),(2),(3)) x"));
    assert_eq!(tree.root, expanded);
    assert!(tree.nodes.iter().any(|node| matches!(node.input, PlanCostInput::Query(query) if std::ptr::eq(query, selected_pointer))));
    assert!(!tree.nodes.iter().any(|node| matches!(node.input, PlanCostInput::Query(query) if std::ptr::eq(query, original_pointer))));
}

#[test]
fn repeated_folded_sources_cost_each_selected_invocation() {
    let mut body = plan(
        "WITH x AS NOT MATERIALIZED (VALUES(1),(2),(3)) SELECT a.column1 FROM x a CROSS JOIN x b",
    );
    let UnifiedPlan::Query(selected) = plan("VALUES(1),(2),(3)") else {
        unreachable!()
    };
    let UnifiedPlan::Query(query) = &mut body else {
        unreachable!()
    };
    let crate::RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    retain_named_input(block.from.as_mut().unwrap(), "x", &selected);
    let statistics = |name: &str| -> Option<RelationStats> {
        panic!("folded values must not read statistics: {name}")
    };
    let estimator = PlanCostEstimator::new(&statistics);
    let tree = estimator
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    let expanded = estimator.estimate(&plan(
        "SELECT a.column1 FROM (VALUES(1),(2),(3)) a CROSS JOIN (VALUES(1),(2),(3)) b",
    ));
    assert_eq!(tree.root, expanded);
    assert_eq!(tree.nodes.iter().filter(|node| matches!(node.input, PlanCostInput::Query(query) if matches!(query.root, crate::RelationalPlan::Values { .. }))).count(), 2);
}

#[test]
fn folded_inputs_keep_their_definition_namespace_before_later_shadowing() {
    let mut body = plan(
        "WITH x AS MATERIALIZED (VALUES(1),(2),(3)) SELECT * FROM (WITH a AS NOT MATERIALIZED (SELECT * FROM x), x AS MATERIALIZED (VALUES(1),(2)) SELECT * FROM x CROSS JOIN a) s",
    );
    let UnifiedPlan::Query(query) = &mut body else {
        unreachable!()
    };
    let crate::RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let SourcePlan::Subquery { body: nested, .. } = block.from.as_mut().unwrap() else {
        unreachable!()
    };
    let selected = nested.ctes[0].body.query().unwrap().clone();
    let crate::RelationalPlan::QueryBlock(block) = &mut nested.root else {
        unreachable!()
    };
    retain_named_input(block.from.as_mut().unwrap(), "a", &selected);
    let statistics = |name: &str| -> Option<RelationStats> {
        panic!("CTE reference became a catalog relation: {name}")
    };
    let tree = PlanCostEstimator::new(&statistics)
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(tree.root.rows, 6.0);
    let mut x_rows = tree.nodes.iter().filter_map(|node| matches!(node.input, PlanCostInput::Source(SourcePlan::Table { name, .. }) if name == "x").then_some(node.cost.rows)).collect::<Vec<_>>();
    x_rows.sort_by(f64::total_cmp);
    assert_eq!(x_rows, [2.0, 3.0]);
}

#[test]
fn unreferenced_writes_still_contribute_one_producer() {
    let statistics = |_: &str| None;
    let estimator = PlanCostEstimator::new(&statistics);
    let body = plan("WITH a AS (INSERT INTO target VALUES(1)) SELECT 7");
    let tree = estimator
        .estimate_nodes_with_control(&body, &ProductionControl::uncontrolled())
        .unwrap();
    assert!(tree.root.execution > estimator.estimate(&plan("SELECT 7")).execution);
    assert_eq!(
        tree.nodes
            .iter()
            .filter(|node| matches!(node.input, PlanCostInput::Command(_)))
            .count(),
        1
    );
}
