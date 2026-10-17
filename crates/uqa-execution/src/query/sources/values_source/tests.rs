//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Option<Vec<u32>>,
}

#[test]
fn values_from_sources_preserve_postgresql_collations_through_qualification() {
    let cases: Vec<Reference> = serde_json::from_str(include_str!(
        "../../relational/values/tests/collations.json"
    ))
    .unwrap();
    assert_eq!(
        cases
            .iter()
            .filter(|case| case.collations.is_some())
            .count(),
        4
    );
    for case in cases {
        let Some(expected) = case.collations else {
            continue;
        };
        let UnifiedPlan::Query(plan) =
            UnifiedPlan::lower(uqa_sql::compile(&case.sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::Values { rows, subqueries } = plan.root else {
            panic!("VALUES")
        };
        assert!(subqueries.is_empty());
        for alias in [None, Some("v".to_string())] {
            let source = SourcePlan::Values {
                rows: rows.clone(),
                alias: alias.clone(),
                column_aliases: alias.as_ref().map(|_| vec!["x".into()]).unwrap_or_default(),
                internal_relation: None,
                internal_column_types: Vec::new(),
                internal_column_collations: Vec::new(),
            };
            let mut operator = build_values_source(
                &Services,
                &Services,
                &source,
                &[],
                &crate::catalog::test_support::empty_scope(),
                || Arc::new(Services),
                None,
                None,
                || panic!("no predicate is placed on the list"),
            )
            .unwrap();
            let name = if alias.is_some() { "x" } else { "column1" };
            for (position, expected) in expected.iter().enumerate() {
                assert_eq!(
                    operator.row_schema().column_collation(position),
                    Some(*expected)
                );
                assert_eq!(operator.row_schema().collation_of(name), Some(*expected));
                if alias.is_some() {
                    assert_eq!(
                        operator.row_schema().qualified_collation("v", name),
                        Some(*expected)
                    );
                }
            }
            let (_, actual) = crate::physical::run_to_rows(operator.as_mut()).unwrap();
            assert_eq!(actual.len(), rows.len());
            let values = actual
                .into_iter()
                .map(|row| vec![row[name].clone()])
                .collect::<Vec<_>>();
            crate::query::relational::values::test_support::assert_values(&case.sql, &values);
        }
    }
}

#[test]
fn values_sources_apply_only_the_predicates_placed_on_their_qualifier() {
    let UnifiedPlan::Query(plan) =
        UnifiedPlan::lower(uqa_sql::compile("VALUES (1), (2), (3)").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::Values { rows, .. } = plan.root else {
        panic!("VALUES")
    };
    let comparison = |qualifier: &str, op, value| ScalarExpr::Binary {
        op,
        lhs: Box::new(ScalarExpr::qualified_column(qualifier, "x")),
        rhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Int(value))),
    };
    let filters = uqa_sql::plan::source_projection::QualifierFilters::from([
        (
            "v".to_string(),
            vec![
                comparison("v", uqa_sql::ast::BinaryOp::Greater, 1),
                comparison("v", uqa_sql::ast::BinaryOp::NotEqual, 3),
            ],
        ),
        (
            "w".to_string(),
            vec![comparison("w", uqa_sql::ast::BinaryOp::Equal, 1)],
        ),
    ]);
    let source = SourcePlan::Values {
        rows,
        alias: Some("v".into()),
        column_aliases: vec!["x".into()],
        internal_relation: None,
        internal_column_types: Vec::new(),
        internal_column_collations: Vec::new(),
    };
    let mut operator = build_values_source(
        &Services,
        &Services,
        &source,
        &[],
        &crate::catalog::test_support::empty_scope(),
        || Arc::new(Services),
        None,
        Some(&filters),
        || crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
    )
    .unwrap();
    let (_, actual) = crate::physical::run_to_rows(operator.as_mut()).unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|row| row["x"].clone())
            .collect::<Vec<_>>(),
        [uqa_core::Value::Int(2)]
    );
}

#[test]
fn values_sources_fail_instead_of_reading_an_unproduced_column() {
    let UnifiedPlan::Query(plan) =
        UnifiedPlan::lower(uqa_sql::compile("VALUES (1, 2), (3, 4)").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::Values { rows, .. } = plan.root else {
        panic!("VALUES")
    };
    let source = SourcePlan::Values {
        rows,
        alias: Some("v".into()),
        column_aliases: vec!["x".into()],
        internal_relation: None,
        internal_column_types: Vec::new(),
        internal_column_collations: Vec::new(),
    };
    let comparison = |column: &str| ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Greater,
        lhs: Box::new(ScalarExpr::qualified_column("v", column)),
        rhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Int(1))),
    };
    let run = |column: &str| {
        let filters = uqa_sql::plan::source_projection::QualifierFilters::from([(
            "v".to_string(),
            vec![comparison(column)],
        )]);
        let mut operator = build_values_source(
            &Services,
            &Services,
            &source,
            &[],
            &crate::catalog::test_support::empty_scope(),
            || Arc::new(Services),
            None,
            Some(&filters),
            || crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
        )
        .unwrap();
        crate::physical::run_to_rows(operator.as_mut()).map(|(_, rows)| rows.len())
    };
    // An aliased column and the default name of an unaliased column are both produced.
    assert_eq!(run("x").unwrap(), 1);
    assert_eq!(run("column2").unwrap(), 2);
    // A whole-row name qualified into a column reads nothing the list produces.
    let error = run("v").unwrap_err().to_string();
    assert!(error.contains("reads v.v"), "{error}");
}
