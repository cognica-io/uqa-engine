//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` generation checks distinguish concrete text output from session-dependent output.

use super::{fixture::Catalog, ColumnType, Statement};

#[test]
fn planned_concatenation_uses_concrete_output_volatility() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("concatenation.json")).unwrap();
    let catalog = Catalog::new("");
    for case in cases {
        let expression = case["expression"].as_str().unwrap();
        let Statement::Select(query) = crate::compile(&format!("SELECT {expression}"))
            .unwrap()
            .remove(0)
        else {
            panic!("scalar expression");
        };
        let schema = crate::RowSchema::with_types(
            vec!["a".into()],
            vec![Some(
                ColumnType::from_sql_name(case["type"].as_str().unwrap()).unwrap(),
            )],
        );
        let scalar = crate::type_resolution::bind_type_introspection_with_resolver(
            crate::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar,
            &schema,
            &[],
            &catalog,
        );
        let actual = catalog
            .context()
            .expression_volatility(&scalar, &schema)
            .unwrap();
        let expected = if case["immutable"].as_bool().unwrap() {
            crate::ast::FunctionVolatility::Immutable
        } else {
            crate::ast::FunctionVolatility::Stable
        };
        assert_eq!(actual, expected, "{case}");
        assert_eq!(
            catalog
                .evaluations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
