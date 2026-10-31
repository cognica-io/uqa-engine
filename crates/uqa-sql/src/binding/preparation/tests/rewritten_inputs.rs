//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{ExpressionPlan, RelationalPlan, SourcePlan};

#[test]
fn membership_inputs_finish_before_enclosing_call_metadata_moves_them() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    for sql in [
        "SELECT pg_typeof(1 IN (NULL, NULL))",
        "SELECT pg_typeof(1 NOT IN ('1', NULL))",
        "SELECT pg_typeof((1 IN (1, 2)) IN (true, false))",
        "SELECT coalesce(1 IN (NULL, NULL), false)",
        "SELECT pg_typeof((SELECT 1) IN (n,n+1)) FROM (VALUES(1)) v(n)",
        "SELECT pg_typeof((SELECT count(*) FROM (SELECT count(*) FROM (VALUES(1),(2)) s(n) GROUP BY DISTINCT GROUPING SETS ((n+NULL),(n*NULL),(n+NULL))) g) IN (n,n+1)) FROM (VALUES(1),(2)) v(n)",
    ] {
        let input = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let context = crate::binding::fixture::empty_binding_context();
        let mut ordinary = input.clone();
        read_prepared_inputs(&NoRoutines, &mut ordinary, &[], &context, None)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let budget = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut actual = read_prepared_inputs_owned(
            &NoRoutines,
            input.copy_for_rewrite(&control).unwrap(),
            &[],
            context.copy_with_control(&control).unwrap(),
            None,
            &control,
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let crate::plan::PlanRef::Query(actual_query) = actual.plan.as_plan_ref() else {
            panic!("query");
        };
        let UnifiedPlan::Query(ordinary_query) = &ordinary else {
            panic!("query");
        };
        assert_eq!(
            serde_json::to_value(actual_query).unwrap(),
            serde_json::to_value(ordinary_query).unwrap(),
            "{sql}"
        );
        ordinary.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| assert!(!matches!(node, ScalarExpr::InList { .. })));
        });
        let check_grouping = |query: &crate::plan::QueryPlan| {
            let Some(grouping) = &query.analyzed_grouping else { return false; };
            let RelationalPlan::QueryBlock(block) = &query.root else { panic!("grouped query"); };
            let mut block = (**block).clone();
            let expected = block.grouping_sets[..2].to_vec();
            grouping.clone().apply(&mut block).unwrap();
            assert_eq!(block.grouping_sets, expected);
            assert!(!block.group_distinct);
            true
        };
        let mut expected_groups = 0;
        ordinary.visit_queries_mut(&mut |query| {
            expected_groups += usize::from(check_grouping(query));
        });
        let mut actual_groups = 0;
        actual.plan.apply_query_metadata(&control, &mut |query| {
            actual_groups += usize::from(check_grouping(query));
            Ok(crate::plan::QueryMetadataUpdate::default())
        }).unwrap();
        assert_eq!(actual_groups, expected_groups, "{sql}");
        if sql.contains("GROUP BY DISTINCT") {
            assert!(actual_groups > 1, "copied occurrences retain completed grouping metadata");
        }
        drop(actual);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

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
