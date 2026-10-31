//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use crate::RowSchemaExecution;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

#[test]
fn values_source_evaluates_each_row_only_when_its_consumer_requests_it() {
    let source = SourcePlan::Values {
        lateral: false,
        rows: vec![
            vec![ScalarExpr::Literal(uqa_core::Value::Int(7))],
            vec![ScalarExpr::Binary {
                op: uqa_sql::ast::BinaryOp::Divide,
                lhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Int(1))),
                rhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Int(0))),
            }],
        ],
        alias: Some("v".into()),
        column_aliases: vec!["n".into()],
        internal_relation: None,
        internal_column_types: vec![],
        internal_column_collations: vec![],
    };
    let scope = crate::catalog::test_support::empty_scope();
    let mut operator = build_values_source(
        &Services,
        &Services,
        &source,
        &[],
        &scope,
        || Ok(Arc::new(Services)),
        None,
        None,
        || panic!("no predicate"),
    )
    .unwrap();
    operator.open().unwrap();
    let first = operator
        .next_with_demand(crate::physical::demand::ONE)
        .unwrap()
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        first.schema.view(&first.rows[0]).get("n"),
        Some(&uqa_core::Value::Int(7))
    );
    assert!(operator
        .next_with_demand(crate::physical::demand::ONE)
        .unwrap_err()
        .to_string()
        .contains("division by zero"));
    operator.close().unwrap();
}

#[test]
fn values_sources_keep_each_outer_row_and_only_expose_their_own_columns() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/lateral_values_oracle.expected.json"
    ))
    .unwrap();
    let reference = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "lateral_rows")
        .unwrap();
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(
        uqa_sql::compile(reference["sql"].as_str().unwrap())
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("query block")
    };
    let Some(SourcePlan::Join { right, .. }) = block.from else {
        panic!("lateral source")
    };
    let mut actual = Vec::new();
    for outer_value in [1, 2] {
        let mut scope = crate::catalog::test_support::empty_scope();
        scope.set_row_lock_outer_row(crate::OwnedPhysicalRow::new(
            crate::RowSchema::with_qualified_types(
                "t",
                vec!["a".into()],
                vec![Some(uqa_sql::ColumnType::Integer)],
            ),
            crate::PhysicalRow::from_values(vec![uqa_core::Value::Int(outer_value)]),
        ));
        let mut operator = build_values_source(
            &Services,
            &Services,
            &right,
            &[],
            &scope,
            || Ok(Arc::new(Services)),
            None,
            None,
            || panic!("no predicate"),
        )
        .unwrap();
        assert_eq!(
            operator.row_schema().qualified_type("v", "x"),
            Some(&uqa_sql::ColumnType::Integer)
        );
        assert!(operator.row_schema().qualified_type("t", "a").is_none());
        let (_, rows) = crate::physical::run_to_rows(operator.as_mut()).unwrap();
        for row in rows {
            let uqa_core::Value::Int(value) = row["x"] else {
                panic!("integer VALUES output")
            };
            actual.push(vec![outer_value.to_string(), value.to_string()]);
        }
    }
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        reference["results"][0]["rows"]
    );
}

#[test]
fn internal_values_capture_a_typed_outer_attribute_in_its_original_scope() {
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let source = SourcePlan::Values {
        lateral: false,
        rows: vec![vec![ScalarExpr::qualified_column("caller", "value")]],
        alias: None,
        column_aliases: vec![],
        internal_relation: Some(relation),
        internal_column_types: vec![None],
        internal_column_collations: vec![],
    };
    let schema = crate::RowSchema::with_qualified_types(
        "caller",
        vec!["value".into()],
        vec![Some(uqa_sql::ColumnType::Integer)],
    );
    let mut scope = crate::catalog::test_support::empty_scope();
    scope.set_row_lock_outer_row(crate::OwnedPhysicalRow::new(
        schema,
        crate::PhysicalRow::from_values(vec![uqa_core::Value::Int(42)]),
    ));
    let mut operator = build_values_source(
        &Services,
        &Services,
        &source,
        &[],
        &scope,
        || Ok(Arc::new(Services)),
        None,
        None,
        || panic!("no predicate"),
    )
    .unwrap();
    assert!(operator.schema().is_empty());
    assert_eq!(
        operator.row_schema().internal_type(relation.column(0)),
        Some(&uqa_sql::ColumnType::Integer)
    );
    operator.open().unwrap();
    let batch = operator.next().unwrap().unwrap();
    let row = crate::OwnedPhysicalRow::new(batch.schema, batch.rows.into_iter().next().unwrap());
    assert_eq!(
        uqa_sql::expr::RowLookup::internal_column(&row, relation.column(0)),
        Some(&uqa_core::Value::Int(42))
    );
    operator.close().unwrap();
}

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
                lateral: false,
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
                || Ok(Arc::new(Services)),
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
        lateral: false,
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
        || Ok(Arc::new(Services)),
        None,
        Some(&filters),
        || {
            Ok(crate::relational::DefaultExpressionEvaluator::shared(
                Vec::new(),
            ))
        },
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
        lateral: false,
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
            || Ok(Arc::new(Services)),
            None,
            Some(&filters),
            || {
                Ok(crate::relational::DefaultExpressionEvaluator::shared(
                    Vec::new(),
                ))
            },
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
