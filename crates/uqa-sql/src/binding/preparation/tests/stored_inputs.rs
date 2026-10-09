//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_outer_identities_preserve_nested_alias_shadowing() {
    let relation = crate::ast::InternalRelationId::allocate();
    let schema = RowSchema::with_qualified_types(
        "source",
        vec!["id".into()],
        vec![Some(ColumnType::SmallInteger)],
    );
    let schema = RowSchema::with_physical_internal_aliases(
        &schema,
        &[(relation.column(0), 0, Some(ColumnType::SmallInteger))],
    );
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(crate::compile(
        "SELECT source.id + (SELECT source.id + (SELECT source.id FROM assignment_target AS source))"
    ).unwrap().remove(0)) else { panic!("query"); };
    let crate::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
        panic!("query block");
    };
    let mut plan = crate::plan::ExpressionPlan {
        scalar: block.projections.remove(0).expr,
        subqueries: block.subqueries,
    };
    crate::binding::bind_expression_plan_routines_for_storage(
        &NoRoutines,
        &mut plan,
        &[],
        &assignment_context(),
        &schema,
    )
    .unwrap();
    let mut outer_inputs = 0;
    let mut local_inputs = 0;
    let mut inspect = |expression: &ScalarExpr| {
        expression.visit(&mut |node| match node {
            ScalarExpr::InternalColumn(column) if *column == relation.column(0) => {
                outer_inputs += 1;
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if qualifier == "source" && column == "id" =>
            {
                local_inputs += 1;
            }
            _ => {}
        });
    };
    inspect(&plan.scalar);
    for query in &plan.subqueries {
        query.visit_scalar_expressions(&mut inspect);
    }
    assert_eq!(outer_inputs, 2);
    assert_eq!(local_inputs, 1);
}

#[test]
fn rule_whole_inputs_expand_only_in_the_event_namespace() {
    for local in [
        "(SELECT new IS NULL FROM (VALUES(NULL::int)) AS new(x))",
        "(SELECT new IS NULL FROM (VALUES(NULL::int)) AS local(new))",
        "(SELECT new.* IS NULL FROM (VALUES(NULL::int)) AS new(x))",
    ] {
        let binding = crate::catalog::events::RuleConditionBinding::for_event(
            &["id".into()],
            crate::ast::RuleEvent::Update,
        );
        let schema = binding.row_schema(&[("id".into(), ColumnType::Integer)]);
        let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
            crate::compile(&format!("SELECT {local} OR NEW IS NULL"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let crate::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("query block");
        };
        let mut plan = crate::plan::ExpressionPlan {
            scalar: block.projections.remove(0).expr,
            subqueries: block.subqueries,
        };
        crate::binding::rule_inputs::expand(
            &NoRoutines,
            &assignment_context(),
            &mut plan,
            &binding,
            "public.event",
            &schema,
        )
        .unwrap();
        assert_eq!(
            binding.referenced_columns(&plan),
            BTreeSet::from([binding.new_column("id").unwrap()])
        );
        let mut casts = 0;
        let mut inspect = |expression: &ScalarExpr| {
            expression.visit(&mut |node| {
                if matches!(node, ScalarExpr::Cast { ty, .. } if ty == "public.event") {
                    casts += 1;
                }
            });
        };
        inspect(&plan.scalar);
        for query in &plan.subqueries {
            query.visit_scalar_expressions(&mut inspect);
        }
        assert_eq!(casts, 1, "{local}");
    }
}
