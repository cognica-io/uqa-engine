//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn owned_qualification_preserves_separate_ordering_scopes_and_exact_lifetimes() {
    let compiled = crate::plan::UnifiedPlan::lower(crate::compile("SELECT sum(x ORDER BY y) FILTER (WHERE z>0), sum(x) FILTER (WHERE z>0) OVER(PARTITION BY p ORDER BY q) FROM t").unwrap().remove(0));
    let crate::plan::UnifiedPlan::Query(query) = compiled else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (index, projection) in block.projections.iter().enumerate() {
        let expected = crate::semantics::qualify_unqualified_columns(&projection.expr, "source");
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(
                qualify_unqualified_columns_with_control(&projection.expr, "source", &control)
                    .unwrap(),
            );
        });
        assert_eq!(output.as_deref(), Some(&expected));
        let mut names = Vec::new();
        output.as_deref().unwrap().visit(&mut |part| match part {
            ScalarExpr::Column(name) => names.push((None, name.to_string(), name.position())),
            ScalarExpr::QualifiedColumn { qualifier, column } => names.push((
                Some(qualifier.clone()),
                column.to_string(),
                column.position(),
            )),
            _ => (),
        });
        let original_positions = {
            let mut positions = Vec::new();
            projection.expr.visit(&mut |part| {
                if let ScalarExpr::Column(name) = part {
                    positions.push(name.position());
                }
            });
            positions
        };
        assert_eq!(
            names
                .iter()
                .map(|(_, _, position)| *position)
                .collect::<Vec<_>>(),
            original_positions
        );
        let columns = names
            .iter()
            .map(|(qualifier, column, _)| (qualifier.as_deref(), column.as_str()))
            .collect::<Vec<_>>();
        if index == 0 {
            assert_eq!(
                columns,
                [(Some("source"), "x"), (None, "y"), (Some("source"), "z")]
            );
        } else {
            assert_eq!(
                columns,
                [
                    (Some("source"), "x"),
                    (Some("source"), "z"),
                    (None, "p"),
                    (None, "q")
                ]
            );
        }
        assert_eq!(allocation.bytes_current, budget.used() as i64);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    token.cancel();
    assert_eq!(
        qualify_unqualified_columns_with_control(&block.projections[0].expr, "source", &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn qualification_accepts_childless_windows_in_both_entry_points() {
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
        crate::compile("SELECT row_number() OVER ()")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let expression = &block.projections[0].expr;
    assert_eq!(
        &crate::semantics::qualify_unqualified_columns(expression, "r"),
        expression
    );
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        &*qualify_unqualified_columns_with_control(expression, "r", &control).unwrap(),
        expression
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn qualification_moves_the_admitted_name_and_parser_position() {
    let name = "x".repeat(4096);
    let source = ScalarExpr::Column(crate::ast::ReferenceName::from(name).with_position(Some(27)));
    let budget = MemoryBudget::new(4097);
    let token = CancellationToken::new();
    for control in [
        ProductionControl::uncontrolled(),
        ProductionControl::new(&budget, &token, &token),
    ] {
        let original = source.copy_for_rewrite(&control).unwrap();
        let ScalarExpr::Column(name) = &*original else {
            panic!("column");
        };
        let allocated = name.as_ptr();
        let qualified = qualify(original, "r", &control).unwrap();
        let ScalarExpr::QualifiedColumn { qualifier, column } = &*qualified else {
            panic!("qualified column");
        };
        assert_eq!(column.as_ptr(), allocated);
        assert_eq!(column.position(), Some(27));
        assert_eq!(qualifier, "r");
        if control.budget().is_some() {
            assert_eq!(qualified.reserved_bytes(), 4097);
            assert_eq!(budget.peak(), 4097);
        }
        drop(qualified);
        assert_eq!(budget.used(), 0);
    }
}
