//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{ExpressionPlan, RelationalPlan, SourcePlan};

#[test]
fn rewritten_subqueries_retain_input_constants_at_the_analyzed_addresses() {
    for (expression, correlated) in [
        ("SELECT n + '1'::integer", true),
        ("SELECT n + '1'::integer FROM (VALUES (50)) local(n)", false),
        (
            "SELECT input.n + '1'::integer FROM (VALUES (50)) input(n)",
            false,
        ),
    ] {
        let sql = format!("SELECT ({expression}) FROM (SELECT 7 AS n) input");
        let mut plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let SourcePlan::Subquery {
            captured_output, ..
        } = block.from.as_mut().unwrap()
        else {
            panic!("source")
        };
        let relation = crate::ast::InternalRelationId::allocate();
        *captured_output = Some(relation);
        let mut input = ExpressionPlan {
            scalar: block.projections[0].expr.clone(),
            subqueries: std::mem::take(&mut block.subqueries),
        };
        let replacement = ExpressionPlan {
            scalar: ScalarExpr::InternalColumn(relation.column(0)),
            subqueries: Vec::new(),
        };
        crate::semantics::view_rewrite::DeferredOuterRewrite::from_inputs(BTreeMap::from([
            (crate::ColumnIdentity::unqualified("n"), replacement.clone()),
            (crate::ColumnIdentity::qualified("input", "n"), replacement),
        ]))
        .bind_expression(&mut input);
        block.projections[0].expr = input.scalar;
        block.subqueries = input.subqueries;
        let context = crate::binding::fixture::empty_binding_context();
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let mut captured = false;
            plan.visit_scalar_expressions(&mut |expression| expression.visit(&mut |node| {
                captured |= matches!(node, ScalarExpr::InternalColumn(column) if column.relation() == relation);
            }));
            assert_eq!(captured, correlated, "{sql}");
            let UnifiedPlan::Query(query) = &plan else {
                panic!("query")
            };
            assert_eq!(
                query.analyzed_output.as_ref().unwrap().column_type(0),
                Some(&ColumnType::Integer)
            );
        }
    }
}
