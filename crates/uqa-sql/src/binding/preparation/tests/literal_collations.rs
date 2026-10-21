//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

#[derive(serde::Deserialize)]
struct Reference {
    query: String,
    null: bool,
    collation: u32,
}

#[test]
fn retained_literal_collations_remain_implicit_across_expression_analysis() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("literal_collations.json")).unwrap();
    assert_eq!(cases.len(), 18);
    let context = crate::binding::fixture::empty_binding_context();
    for case in cases {
        let mut plan = UnifiedPlan::lower(crate::compile(&case.query).unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |expression| {
            let ScalarExpr::Column(name) = expression else {
                return;
            };
            let (value, oid) = match name.as_str() {
                "c" => ("x", 950),
                "p" => ("y", 951),
                _ => panic!("fixture column"),
            };
            *expression = ScalarExpr::TypedLiteral {
                value: if case.null {
                    Value::Null
                } else {
                    Value::Str(value.into())
                },
                ty: ColumnType::Text.catalog_name(),
                bound_type: Some(ColumnType::Text),
                collation_oid: Some(oid),
                composite_source: None,
                parameter_index: None,
            };
        });
        let encoded = serde_json::to_string(&plan).unwrap();
        let restored: UnifiedPlan = serde_json::from_str(&encoded).unwrap();
        for plan in [&plan, &restored] {
            let UnifiedPlan::Query(query) = plan else {
                panic!("query")
            };
            for output in [
                crate::binding::analyze_query_plan_schema(&NoRoutines, query, &[], &context, None),
                crate::binding::bind_query_plan_schema(&NoRoutines, query, &[], &context, None),
            ] {
                assert_eq!(
                    output.unwrap().column_collation(0),
                    Some(case.collation),
                    "{}",
                    case.query
                );
            }
        }
    }
}

#[test]
fn literal_lowering_and_legacy_codecs_preserve_selected_or_default_collations() {
    use uqa_core::memory::{MemoryBudget, ProductionControl};
    use uqa_core::CancellationToken;
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &token, &token);
    for collation_oid in [None, Some(0), Some(100), Some(950), Some(951)] {
        let source = crate::ast::Expr::TypedLiteral {
            value: Value::Str("x".into()),
            ty: ColumnType::Text.catalog_name(),
            collation_oid,
            composite_source: None,
        };
        let encoded = serde_json::to_string(&source).unwrap();
        if collation_oid.is_none() {
            assert!(!encoded.contains("collation_oid"));
        }
        let restored = serde_json::from_str(&encoded).unwrap();
        let plan = crate::plan::ExpressionPlan::lower(restored);
        assert_eq!(
            RowSchema::default().input_collation(&plan.scalar),
            collation_oid
        );
        let cloned = plan.scalar.clone_with_control(&control).unwrap();
        assert_eq!(*cloned, plan.scalar);
        drop(cloned);
        assert_eq!(budget.used(), 0);
        // Borrowed AST lowering uses the same admitted projection as owned lowering.
        let cloned =
            crate::plan::ExpressionPlan::lower_column_budgeted(&source, &budget, &token, &token)
                .unwrap();
        assert_eq!(*cloned, plan.scalar);
        drop(cloned);
        assert_eq!(budget.used(), 0);
        let mut preparation = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope:
                &mut SchemaScope::for_analysis(&crate::binding::fixture::empty_binding_context())
                    .unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let observed = preparation
            .expression(&plan.scalar, &RowSchema::default(), &[])
            .unwrap();
        assert_eq!(
            observed.derivation.oid(),
            i64::from(collation_oid.unwrap_or(100))
        );
    }
}

#[test]
fn literal_analysis_rejects_collations_on_noncollatable_types() {
    let context = crate::binding::fixture::empty_binding_context();
    let mut scope = SchemaScope::for_analysis(&context).unwrap();
    let mut preparation = Preparation {
        bound_condition_queries: None,
        routines: &NoRoutines,
        scope: &mut scope,
        parameters: ParameterTypes::new(&[]),
        schema_expression: None,
    };
    let expression = ScalarExpr::TypedLiteral {
        value: Value::Int(1),
        ty: ColumnType::Integer.catalog_name(),
        bound_type: Some(ColumnType::Integer),
        collation_oid: Some(950),
        composite_source: None,
        parameter_index: None,
    };
    let Err(error) = preparation.expression(&expression, &RowSchema::default(), &[]) else {
        panic!("noncollatable constant accepted a collation");
    };
    assert!(error
        .to_string()
        .contains("constant collation does not match"));
}
