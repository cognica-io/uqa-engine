//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::CancellationToken;

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
}

fn estimate(sql: &str) -> PlanCost {
    let statistics = |_: &str| {
        Some(RelationStats {
            row_count: 1000,
            ..Default::default()
        })
    };
    PlanCostEstimator::new(&statistics).estimate(&plan(sql))
}

#[test]
fn blocking_work_and_limit_use_the_shared_native_model() {
    let scan = estimate("SELECT id FROM items");
    let limit = estimate("SELECT id FROM items LIMIT 10 OFFSET 20");
    assert_eq!(scan.rows, 1000.0);
    assert_eq!(scan.startup, 0.0);
    assert_eq!(limit.rows, 10.0);
    assert!(limit.startup > 0.0 && limit.startup < limit.execution);
    assert!(limit.execution < scan.execution);
    let sorted = estimate("SELECT id FROM items ORDER BY id LIMIT 10");
    assert_eq!(sorted.rows, 10.0);
    assert!(sorted.startup >= scan.execution && sorted.startup <= sorted.execution);
    let aggregate = estimate("SELECT count(*) FROM items");
    assert_eq!(aggregate.rows, 1.0);
    assert!(aggregate.startup >= scan.execution && aggregate.startup <= aggregate.execution);
    for sql in [
        "SELECT a.id FROM items a JOIN items b ON a.id=b.id",
        "SELECT id FROM items UNION SELECT id FROM items",
        "WITH x AS MATERIALIZED (SELECT id FROM items ORDER BY id) SELECT * FROM x LIMIT 2",
        "SELECT (SELECT count(*) FROM items) FROM items LIMIT 5",
    ] {
        let cost = estimate(sql);
        assert!(
            cost.startup.is_finite() && cost.startup >= 0.0 && cost.startup <= cost.execution,
            "{sql}: {cost:?}"
        );
    }
}

#[test]
fn cost_scopes_borrow_names_and_release_rejected_allocations() {
    let ctes = (0..128)
        .map(|index| format!("c{index} AS (SELECT 1 AS x)"))
        .collect::<Vec<_>>()
        .join(",");
    let body = plan(&format!("WITH {ctes} SELECT * FROM c127"));
    let statistics = |_: &str| None;
    let estimator = PlanCostEstimator::new(&statistics);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(256);
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        estimator
            .estimate_with_control(&body, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            estimator
                .estimate_with_control(&body, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    let budget = MemoryBudget::new(256 * 1024);
    assert_eq!(
        estimator
            .estimate_with_control(
                &body,
                &ProductionControl::new(&budget, &original, &invoking)
            )
            .unwrap()
            .rows,
        1.0
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn hash_startup_respects_inner_and_outer_build_ownership_for_unequal_inputs() {
    let statistics = |name: &str| {
        Some(RelationStats {
            row_count: if name == "small" { 1 } else { 10_000 },
            ..Default::default()
        })
    };
    let estimator = PlanCostEstimator::new(&statistics);
    for (left, right) in [("small", "large"), ("large", "small")] {
        for join in ["JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN"] {
            let cost = estimator.estimate(&plan(&format!(
                "SELECT a.id FROM {left} a {join} {right} b ON a.id=b.id"
            )));
            assert!(
                cost.startup > 0.0 && cost.startup <= cost.execution,
                "{left} {join} {right}: {cost:?}"
            );
        }
    }
}
