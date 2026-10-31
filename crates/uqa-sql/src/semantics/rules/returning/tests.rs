//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::ReturningAliases,
    plan::{ProjectionPlan, QueryPlan, UnifiedPlan},
    ScalarExpr,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn query(sql: &str) -> QueryPlan {
    let statement = crate::compile(sql).unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query fixture");
    };
    *query
}

#[test]
fn returning_image_inspection_borrows_large_values_and_names_with_no_allowance() {
    let query = query(&format!(
        "SELECT old.n, '{}' FROM items AS \"{}\"",
        "x".repeat(4 * 1024 * 1024),
        "q".repeat(4096)
    ));
    let subqueries = [query];
    let returning = [ProjectionPlan {
        expr: ScalarExpr::ScalarSubquery(0),
        alias: None,
    }];
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &original, &invoking);
    let aliases = ReturningAliases::default();
    let observed = allocation_counter::measure(|| {
        let request = RuleReturningRequest::from_plan_with_control(
            &returning,
            &aliases,
            &subqueries,
            &control,
        )
        .unwrap();
        assert!(request.captures());
        assert_eq!(request.images, RuleReturningImages::OLD);
    });
    assert_eq!(observed.bytes_total, 0, "{observed:?}");
    assert_eq!(memory.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        for returning in [returning.as_slice(), &[]] {
            assert_eq!(
                RuleReturningRequest::from_plan_with_control(
                    returning,
                    &aliases,
                    &subqueries,
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
        }
        token.reset();
    }
    assert!(RuleReturningRequest::from_plan_with_control(
        &returning,
        &aliases,
        &subqueries,
        &control
    )
    .unwrap()
    .captures());
}

#[test]
fn returning_image_selection_preserves_lexical_and_lateral_shadowing() {
    let aliases = ReturningAliases::default();
    let returning = [ProjectionPlan {
        expr: ScalarExpr::ScalarSubquery(0),
        alias: None,
    }];
    for (sql, mask) in [
        ("SELECT old.n, new.n", 6),
        ("SELECT old.n FROM items old", 0),
        ("SELECT new.n FROM items old", 4),
        ("SELECT n FROM items old", 1),
        ("SELECT q.n FROM (SELECT old.n) q", 2),
        (
            "SELECT 1 FROM items old CROSS JOIN LATERAL (SELECT old.n) q",
            0,
        ),
        ("SELECT 1 FROM items old CROSS JOIN (SELECT old.n) q", 2),
        ("SELECT 1 FROM items old JOIN items new ON old.n = new.n", 0),
        ("WITH q AS (SELECT old.n) SELECT 1 FROM items old", 2),
        (
            "SELECT old.n FROM (items old JOIN items new ON old.n = new.n) joined",
            2,
        ),
        ("SELECT old.n FROM items \"OLD\"", 0),
        ("SELECT old.n UNION ALL SELECT new.n", 6),
    ] {
        let request = RuleReturningRequest::from_plan(&returning, &aliases, &[query(sql)]);
        assert_eq!(request.images.0, mask, "{sql}");
    }
}

#[test]
fn returning_image_selection_follows_only_selected_scalar_subqueries() {
    let aliases = ReturningAliases::default();
    let subqueries = [query("SELECT old.n"), query("SELECT new.n")];
    let returning = |expr| [ProjectionPlan { expr, alias: None }];
    let request = |expr| RuleReturningRequest::from_plan(&returning(expr), &aliases, &subqueries);
    assert_eq!(request(ScalarExpr::ScalarSubquery(0)).images.0, 2);
    assert_eq!(request(ScalarExpr::ScalarSubquery(1)).images.0, 4);
    assert_eq!(request(ScalarExpr::ScalarSubquery(9)).images.0, 0);
    assert_eq!(request(ScalarExpr::Literal(Value::Int(1))).images.0, 0);
    assert!(!RuleReturningRequest::from_plan(&[], &aliases, &subqueries).captures());
}
