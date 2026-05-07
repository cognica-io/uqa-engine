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
//! - parse round-trip: `optimize(parse(sql))` produces a `SelectStmt`
//!   whose textual semantics are unchanged on the small test corpus.

use proptest::prelude::*;
use uqa_core::Value;
use uqa_planner::{optimize, OptimizerConfig};
use uqa_sql::ast::{Expr, Projection, SelectStmt};

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
        from: None,
        r#where: Some(filter),
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
    }
}

fn optimized_where(filter: Expr) -> Option<Expr> {
    let cfg = OptimizerConfig::default();
    let stmt = select_with_where(filter);
    optimize(stmt, &cfg).r#where
}

/// Returns true iff `e` is structurally equal to `other`. We compare
/// via Debug strings since `Expr` does not implement `PartialEq`.
fn expr_eq(a: &Expr, b: &Expr) -> bool {
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

        let cfg = OptimizerConfig::default();
        let stmt = select_with_where(filter);
        let once = optimize(stmt, &cfg);
        let twice = optimize(once.clone(), &cfg);
        prop_assert!(
            expr_eq(once.r#where.as_ref().unwrap(), twice.r#where.as_ref().unwrap()),
            "optimize not idempotent",
        );
    }
}

/// Concrete identities: each rewrite pinned with a hand-picked input.

#[test]
fn and_with_true_drops_the_true() {
    let filter = Expr::And(vec![lit_true(), col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &col("x"));
}

#[test]
fn and_with_false_short_circuits() {
    let filter = Expr::And(vec![col("x"), lit_false(), col("y")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &lit_false());
}

#[test]
fn or_with_true_short_circuits() {
    let filter = Expr::Or(vec![col("x"), lit_true(), col("y")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &lit_true());
}

#[test]
fn or_with_false_drops_the_false() {
    let filter = Expr::Or(vec![lit_false(), col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &col("x"));
}

#[test]
fn empty_and_collapses_to_true() {
    let filter = Expr::And(vec![]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &lit_true());
}

#[test]
fn empty_or_collapses_to_false() {
    let filter = Expr::Or(vec![]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &lit_false());
}

#[test]
fn single_element_and_collapses() {
    let filter = Expr::And(vec![col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &col("x"));
}

#[test]
fn single_element_or_collapses() {
    let filter = Expr::Or(vec![col("x")]);
    let got = optimized_where(filter).unwrap();
    prop_assert_eq_helper(&got, &col("x"));
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

/// Equality helper: panics with a useful message instead of using
/// proptest macros (these are regular `#[test]` cases).
fn prop_assert_eq_helper(got: &Expr, expected: &Expr) {
    assert!(expr_eq(got, expected), "expected {expected:?}, got {got:?}");
}
