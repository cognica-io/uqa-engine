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
        let schema =
            binding.row_schema(&[crate::ast::ColumnDef::nullable("id", ColumnType::Integer)]);
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

#[test]
fn restored_query_layouts_preserve_syntax_scopes_and_parameter_types() {
    for sql in [
        "SELECT s.k,$1 AS p,outer_scope.x FROM (SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)) s WHERE s.k=(SELECT 2)",
        "WITH c(x) AS (SELECT 2::smallint) SELECT s.k FROM (SELECT x AS k FROM c) s CROSS JOIN (SELECT 1 AS x) c WHERE s.k>0",
        "SELECT x FROM (SELECT NULL AS x UNION ALL SELECT 2::smallint) s WHERE x>0",
    ] {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) else {
            panic!("query");
        };
        let before = serde_json::to_value(&query).unwrap();
        let outer = RowSchema::with_qualified_types(
            "outer_scope", vec!["x".into()], vec![Some(ColumnType::BigInteger)],
        );
        restore_query_layouts(
            &NoRoutines,
            &mut query,
            &[crate::SQLParam::typed_scalar(uqa_core::Value::Int(9), ColumnType::BigInteger)],
            &crate::binding::fixture::empty_binding_context(),
            Some(&outer),
        ).unwrap();
        assert_eq!(serde_json::to_value(&query).unwrap(), before, "{sql}");
        query.visit_queries_mut(&mut |query| {
            assert!(query.analyzed_output.is_some(), "{sql}");
            if matches!(&query.root, crate::plan::RelationalPlan::QueryBlock(block) if block.from.is_some()) {
                assert!(query.analyzed_source.is_some(), "{sql}");
            }
        });
        if sql.contains("AS p") {
            let output = query.analyzed_output.as_ref().unwrap();
            assert_eq!(output.column_type(1), Some(&ColumnType::BigInteger));
            assert_eq!(output.column_type(2), Some(&ColumnType::BigInteger));
        }
    }
}
