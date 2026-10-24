//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed clause bindings belong to one unchanged lexical scope.

use super::{column, BTreeMap, ColumnType, EmptyRoutineResolution, RelationIdentity, RowSchema};
use crate::binding::{
    fixture::{catalog, empty_binding_context, table_definition},
    BindingContext, SchemaScope, SOURCE_BINDINGS,
};
use crate::plan::{RelationalPlan, UnifiedPlan};

#[test]
fn repeated_grouping_references_bind_a_scalar_query_once() {
    let context = BindingContext {
        catalog: catalog(BTreeMap::from([(
            RelationIdentity::new("public", "input"),
            table_definition(vec![column("x", ColumnType::Integer)]),
        )])),
        ..empty_binding_context()
    };
    for copies in [1, 8, 64] {
        let sets = std::iter::repeat_n("(value)", copies)
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT (SELECT x FROM input) AS value GROUP BY GROUPING SETS ({sets})");
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
        else {
            panic!("SELECT query");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block");
        };
        assert_eq!(block.subqueries.len(), 1);
        let mut scope = SchemaScope::for_analysis(&context).unwrap();
        SOURCE_BINDINGS.with(|count| count.set(0));
        scope
            .validate_query_block_clauses(
                &EmptyRoutineResolution,
                block,
                &RowSchema::default(),
                &RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Integer)]),
                &[],
                None,
            )
            .unwrap();
        assert_eq!(
            SOURCE_BINDINGS.with(std::cell::Cell::get),
            1,
            "{copies} grouping references"
        );
    }
}

#[test]
fn retained_query_results_require_the_complete_outer_descriptor() {
    use crate::binding::preparation::BLOCK_ANALYSES;
    use crate::binding::type_resolution::ValidatedSubqueries;
    use crate::ScalarExpr;

    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT (SELECT outer_row.x) AS value")
            .unwrap()
            .remove(0),
    ) else {
        panic!("SELECT query");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block");
    };
    let context = empty_binding_context();
    let mut scope = SchemaScope::for_analysis(&context).unwrap();
    let queries =
        ValidatedSubqueries::new(&EmptyRoutineResolution, &block.subqueries, &[]).retaining();
    let expression = [ScalarExpr::ScalarSubquery(0)];
    BLOCK_ANALYSES.with(|count| count.set(0));
    for (ty, analyses) in [
        (ColumnType::Integer, 1),
        (ColumnType::Integer, 1),
        (ColumnType::Text, 2),
        (ColumnType::Integer, 3),
    ] {
        let input =
            RowSchema::with_qualified_types("outer_row", vec!["x".into()], vec![Some(ty.clone())]);
        let result = queries.resolver(&mut scope, &expression, &input).unwrap();
        let output = result.scalar_subquery_schemas.as_ref().unwrap()[0]
            .as_ref()
            .unwrap();
        assert_eq!(output.column_type(0), Some(&ty));
        assert_eq!(BLOCK_ANALYSES.with(std::cell::Cell::get), analyses);
    }
    let wrong_qualifier = RowSchema::with_qualified_types(
        "other_row",
        vec!["x".into()],
        vec![Some(ColumnType::Integer)],
    );
    assert!(queries
        .resolver(&mut scope, &expression, &wrong_qualifier)
        .is_err());
    let valid = RowSchema::with_qualified_types(
        "outer_row",
        vec!["x".into()],
        vec![Some(ColumnType::Integer)],
    );
    assert!(queries.resolver(&mut scope, &expression, &valid).is_ok());
}

#[test]
fn clause_results_do_not_cross_parameters_or_cte_scopes() {
    use crate::binding::analyze_query_plan_schema;
    use crate::SQLParam;
    use uqa_core::Value;

    let context = empty_binding_context();
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT (SELECT $1) AS value GROUP BY GROUPING SETS ((value), (value))")
            .unwrap()
            .remove(0),
    ) else {
        panic!("SELECT query");
    };
    for (value, ty) in [
        (Value::Int(7), ColumnType::Integer),
        (Value::Str("seven".into()), ColumnType::Text),
    ] {
        let params = [SQLParam::typed_scalar(value, ty.clone())];
        let output =
            analyze_query_plan_schema(&EmptyRoutineResolution, &query, &params, &context, None)
                .unwrap();
        assert_eq!(output.column_type(0), Some(&ty));
    }
    for (sql, ty) in [
        ("WITH input AS (SELECT 1 AS x) SELECT (SELECT x FROM input) AS value GROUP BY GROUPING SETS ((value), (value))", ColumnType::Integer),
        ("WITH input AS (SELECT 'outer'::text AS x) SELECT (WITH input AS (SELECT 1 AS x) SELECT x FROM input) AS value GROUP BY GROUPING SETS ((value), (value))", ColumnType::Integer),
        ("WITH input AS (SELECT 'outer'::text AS x) SELECT (SELECT x FROM input) AS value GROUP BY GROUPING SETS ((value), (value))", ColumnType::Text),
    ] {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) else { panic!("SELECT query"); };
        let output = analyze_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None).unwrap();
        assert_eq!(output.column_type(0), Some(&ty), "{sql}");
    }
}

#[test]
fn clause_query_bindings_preserve_postgresql_descriptors_and_error_order() {
    use crate::binding::analyze_query_plan_schema;
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_clause_query_results.json")).unwrap();
    let context = BindingContext {
        catalog: catalog(BTreeMap::from([(
            RelationIdentity::new("public", "input"),
            table_definition(vec![column("x", ColumnType::Integer)]),
        )])),
        ..empty_binding_context()
    };
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("SELECT query");
        };
        let result =
            analyze_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None);
        if let Some(expected) = case.get("error") {
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                expected["sqlstate"].as_str(),
                "{sql}: {error}"
            );
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{sql}"
            );
        } else {
            let output = result.unwrap();
            let columns = case["columns"].as_array().unwrap();
            assert_eq!(output.len(), columns.len(), "{sql}");
            for (index, column) in columns.iter().enumerate() {
                assert_eq!(
                    output.columns()[index],
                    column[0].as_str().unwrap(),
                    "{sql}"
                );
                assert_eq!(
                    output.column_type(index).unwrap().regtype_name(),
                    column[1].as_str().unwrap(),
                    "{sql}"
                );
            }
        }
    }
}
