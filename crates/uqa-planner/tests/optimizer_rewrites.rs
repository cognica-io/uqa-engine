//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optimizer rewrite property tests (Master Plan Section 2.3).
//!
//! Pins the algebraic rewrites the optimizer ships:
//! - Boolean simplification: `True AND x == x`, `False AND x == False`,
//!   `True OR x == True`, `False OR x == x`,
//! - single-element `And` / `Or` collapse to the inner expr,
//! - empty `And` collapses to `True`, empty `Or` collapses to `False`,
//! - **idempotence**: `optimize(optimize(s)) == optimize(s)` (the
//!   rewriter is a fixed-point operator),
//! - lowering boundary: optimization consumes `UnifiedPlan` / `ScalarExpr`
//!   and never reconstructs a parser statement.

use proptest::prelude::*;
use uqa_core::Value;
use uqa_planner::{
    optimize, ExpressionPlan, OptimizerConfig, RelationalPlan, ScalarExpr, UnifiedPlan,
};
use uqa_sql::ast::{Expr, Projection, SelectStmt, Statement};

fn lit_true() -> Expr {
    Expr::Literal(Value::Bool(true))
}

fn lit_false() -> Expr {
    Expr::Literal(Value::Bool(false))
}

fn col(name: &str) -> Expr {
    Expr::Column(name.into())
}

/// Build a minimal SELECT with a single WHERE clause.
fn select_with_where(filter: Expr) -> SelectStmt {
    SelectStmt {
        projections: vec![Projection {
            alias: None,
            expr: col("id"),
        }],
        values: Vec::new(),
        from: None,
        r#where: Some(filter),
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
        locking: Vec::new(),
        windows: Vec::new(),
    }
}

fn optimize_select(stmt: SelectStmt) -> UnifiedPlan {
    let cfg = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    optimize(UnifiedPlan::lower(Statement::Select(Box::new(stmt))), &cfg)
        .expect("optimizer succeeds")
}

fn plan_where(plan: &UnifiedPlan) -> Option<&ScalarExpr> {
    let UnifiedPlan::Query(query) = plan else {
        return None;
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return None;
    };
    block.r#where.as_ref()
}

fn optimized_where(filter: Expr) -> Option<ScalarExpr> {
    let plan = optimize_select(select_with_where(filter));
    plan_where(&plan).cloned()
}

fn physical(expression: Expr) -> ScalarExpr {
    ExpressionPlan::lower(expression).scalar
}

/// Returns true iff `e` is structurally equal to `other`. We compare
/// via Debug strings since `Expr` does not implement `PartialEq`.
fn expr_eq(a: &ScalarExpr, b: &ScalarExpr) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// `optimize(optimize(stmt)) == optimize(stmt)` for any small
    /// SELECT we can build by hand. The rewriter is a fixed-point
    /// operator, so applying it a second time changes nothing.
    #[test]
    fn idempotent(
        n_extras in 0u32..=3,
    ) {
        // Build an `And` with a varying number of literal-true noise
        // members surrounding a column ref, plus one OR with a noise
        // false. The optimizer should reduce both to a clean shape,
        // and a second pass should not change anything.
        let mut and_parts = vec![col("id")];
        for _ in 0..n_extras {
            and_parts.push(lit_true());
        }
        let or_with_false = Expr::Or(vec![lit_false(), col("name")]);
        let filter = Expr::And(vec![Expr::And(and_parts), or_with_false]);

        let cfg = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
        let stmt = select_with_where(filter);
        let once = optimize(UnifiedPlan::lower(Statement::Select(Box::new(stmt))), &cfg)
            .expect("first optimizer pass succeeds");
        let twice = optimize(once.clone(), &cfg).expect("second optimizer pass succeeds");
        prop_assert!(
            expr_eq(plan_where(&once).unwrap(), plan_where(&twice).unwrap()),
            "optimize not idempotent",
        );
    }
}

/// Concrete identities: each rewrite pinned with a hand-picked input.

#[test]
fn and_with_true_drops_the_true() {
    let filter = Expr::And(vec![lit_true(), col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(col("x")));
}

#[test]
fn and_with_false_short_circuits() {
    let filter = Expr::And(vec![col("x"), lit_false(), col("y")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(lit_false()));
}

#[test]
fn or_with_true_short_circuits() {
    let filter = Expr::Or(vec![col("x"), lit_true(), col("y")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(lit_true()));
}

#[test]
fn or_with_false_drops_the_false() {
    let filter = Expr::Or(vec![lit_false(), col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(col("x")));
}

#[test]
fn empty_and_collapses_to_true() {
    let filter = Expr::And(vec![]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(lit_true()));
}

#[test]
fn empty_or_collapses_to_false() {
    let filter = Expr::Or(vec![]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(lit_false()));
}

#[test]
fn single_element_and_collapses() {
    let filter = Expr::And(vec![col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(col("x")));
}

#[test]
fn single_element_or_collapses() {
    let filter = Expr::Or(vec![col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &physical(col("x")));
}

#[test]
fn nested_and_flattens() {
    // And([And([x, y]), z]) should produce a single flattened And
    // with no nested And node remaining.
    let filter = Expr::And(vec![Expr::And(vec![col("x"), col("y")]), col("z")]);
    let got = optimized_where(filter).unwrap();
    let dbg = format!("{got:?}");
    // Should mention "And" exactly once (the outer one).
    let occurrences = dbg.matches("And(").count();
    assert_eq!(occurrences, 1, "expected flattened And, got {dbg}");
}

#[test]
fn unused_window_constants_keep_postgresql_planning_errors() {
    for sql in [
        "SELECT 1 WINDOW unused AS (ORDER BY 1/0)",
        "SELECT 1 WINDOW unused AS (ORDER BY (SELECT 1/0))",
    ] {
        let statement = uqa_sql::compile(sql).unwrap().remove(0);
        let error = optimize(
            UnifiedPlan::lower(statement),
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
        )
        .unwrap_err();
        // Independently captured in named_window_viewdef_oracle.expected.json.
        let uqa_planner::optimizer::OptimizerError::Expression(error) = error else {
            panic!("expected expression error: {sql}: {error}")
        };
        assert_eq!(error.sqlstate(), Some("22012"), "{sql}: {error}");
    }
}

#[test]
fn unused_window_aggregate_still_selects_aggregate_execution() {
    let Statement::Select(select) = uqa_sql::compile(
        "SELECT 1 FROM (VALUES(1),(2)) AS t(v) WINDOW unused AS (ORDER BY sum(v))",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected SELECT")
    };
    let UnifiedPlan::Query(query) = optimize_select(*select) else {
        panic!("expected query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("expected query block")
    };
    assert!(matches!(
        block.compute,
        uqa_sql::plan::ComputePlan::Aggregate
    ));
}

#[test]
fn window_definitions_follow_subquery_remapping_and_source_constants() {
    let Statement::Select(select) = uqa_sql::compile(
        "SELECT row_number() OVER w FROM (VALUES(2)) AS source(v) WINDOW w AS (ORDER BY v + 1), unused AS (ORDER BY (SELECT 3))",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected SELECT")
    };
    let plan = optimize_select(*select);
    let UnifiedPlan::Query(query) = &plan else {
        panic!("expected query")
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block")
    };
    assert_eq!(
        block.windows[0].spec.order_by[0].expr,
        ScalarExpr::Literal(Value::Int(3))
    );
    let ScalarExpr::WindowCall { spec, .. } = &block.projections[0].expr else {
        panic!("expected window call")
    };
    assert_eq!(spec.order_by, block.windows[0].spec.order_by);
    let ScalarExpr::ScalarSubquery(id) = block.windows[1].spec.order_by[0].expr else {
        panic!("expected retained unused window subquery")
    };
    assert!(id < block.subqueries.len());
    let repeated = optimize(
        plan.clone(),
        &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(repeated).unwrap(),
        serde_json::to_value(plan).unwrap()
    );
}

/// Equality helper: panics with a useful message instead of using
/// proptest macros (these are regular `#[test]` cases).
fn prop_assert_eq_helper(got: &ScalarExpr, expected: &ScalarExpr) {
    assert!(expr_eq(got, expected), "expected {expected:?}, got {got:?}");
}
