//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::ExpressionPlan, schema::retention::CatalogRetentionError};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn expression(sql: &str) -> Expr {
    let crate::ast::Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT expression");
    };
    query.projections.remove(0).expr
}

fn visit_owned_children(
    node: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    node.map_children(control, |_, child| visit_owned_children(child, control))
}

#[test]
fn lowering_retains_exact_child_owners_for_every_scalar_container() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for sql in [
        "SELECT a",
        "SELECT t.a",
        "SELECT 'literal'",
        "SELECT $1",
        "SELECT concat(a, '한🙂')",
        "SELECT ARRAY[1, 2, 3]",
        "SELECT ROW(a, 'text')",
        "SELECT a + 1",
        "SELECT -a",
        "SELECT NOT a",
        "SELECT a AND b AND c",
        "SELECT a OR b OR c",
        "SELECT a IS NOT NULL",
        "SELECT a BETWEEN 1 AND 2",
        "SELECT a NOT IN (1, 2, 3)",
        "SELECT CASE a WHEN 1 THEN 'first' ELSE 'last' END",
        "SELECT CASE WHEN a THEN 'yes' END",
        "SELECT CAST(a AS text)",
        "SELECT sum(a ORDER BY b DESC NULLS FIRST) FILTER (WHERE c)",
        "SELECT sum(a) OVER (PARTITION BY b ORDER BY c ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
    ] {
        let source = expression(sql);
        let expected = ExpressionPlan::lower(source.clone());
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(ExpressionPlan::lower_scalar_for_rewrite(&source, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(*output, expected.scalar, "{sql}");
        assert_eq!(output.reserved_bytes(), budget.used(), "{sql}");
        assert_eq!(
            measured.bytes_current,
            i64::try_from(budget.used()).unwrap(),
            "{sql}"
        );
        let output = visit_owned_children(output, &control).unwrap();
        let frozen = output.finish(&control).unwrap();
        assert_eq!(*frozen, expected.scalar, "{sql}");
        assert_eq!(budget.used(), frozen.reserved_bytes(), "{sql}");
        drop(frozen);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn direct_lowering_selects_a_child_without_retaining_discarded_payloads() {
    let source = Expr::Row(vec![
        Expr::Literal(Value::Str("a".repeat(4096))),
        Expr::Not(Box::new(Expr::Array(vec![
            Expr::Literal(Value::Str("b".repeat(257))),
            Expr::Literal(Value::Str("c".repeat(8192))),
        ]))),
    ]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = ExpressionPlan::lower_scalar_for_rewrite(&source, &control).unwrap();
    let before = budget.used();
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        selected = Some(output.select_child(1).select_child(0).select_child(0));
    });
    let selected = selected.unwrap();
    assert_eq!(*selected, ScalarExpr::Literal(Value::Str("b".repeat(257))));
    assert_eq!(budget.used(), 257);
    assert_eq!(measured.count_total, 0);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - 257).unwrap()
    );
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn direct_lowering_releases_partial_siblings_and_rejects_both_cancellations() {
    let source = Expr::Row(vec![
        Expr::Literal(Value::Str("a".repeat(97))),
        Expr::Literal(Value::Str("b".repeat(4 << 20))),
    ]);
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            ExpressionPlan::lower_scalar_for_rewrite(&source, &control),
            Err(CatalogRetentionError::Memory(_))
        ));
    });
    assert!(measured.bytes_max <= 4096, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(budget.used(), 0);

    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(control.check().is_err());
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                ExpressionPlan::lower_scalar_for_rewrite(&source, &control),
                Err(CatalogRetentionError::Cancelled(_))
            ));
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn direct_lowering_rejects_query_children_without_publishing_partial_owners() {
    let source = expression("SELECT ROW('retained sibling', (SELECT 1))");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(matches!(
        ExpressionPlan::lower_scalar_for_rewrite(&source, &control),
        Err(CatalogRetentionError::UnexpectedSubquery)
    ));
    assert_eq!(budget.used(), 0);
}
