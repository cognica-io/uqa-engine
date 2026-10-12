//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named ROW analysis reports field coercion and width failures in `PostgreSQL` order.

use super::{expressions, Catalog, ColumnType, Fixture, Routines, RowSchema, Statement};

#[test]
fn named_row_cast_diagnostics_match_postgresql_in_analysis_binding_and_encoding() {
    let fixture: Fixture = serde_json::from_str(include_str!("../../named_rows.json")).unwrap();
    let catalog = Catalog(&fixture.states[0].catalog, true);
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../named_row_errors.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let base = case["base"].as_str().unwrap();
        let check = case["check"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(base).unwrap();
        let Statement::CreateDomain(mut domain) =
            crate::compile(&format!("CREATE DOMAIN d AS {base} CHECK({check})"))
                .unwrap()
                .remove(0)
        else {
            panic!("domain declaration")
        };
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&ty),
            types: Some(&catalog),
            routines: &Routines,
        };
        let expression = &mut domain.checks[0].expression;
        let encoded = context.check(expression).map(drop);
        let stored = super::stored::retain(expression, &schema, &catalog);
        let mut plan = crate::plan::UnifiedPlan::lower(
            crate::compile(&format!("SELECT {}", check.replace("VALUE", "$1")))
                .unwrap()
                .remove(0),
        );
        let analyzed = crate::binding::infer_prepared_parameter_types(
            &catalog,
            &plan,
            &[Some(ty.clone())],
            &crate::binding::fixture::empty_binding_context(),
        )
        .map(drop);
        let prepared = crate::binding::read_prepared_inputs(
            &catalog,
            &mut plan,
            &[Some(ty)],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .map(drop);
        for (owner, result) in [
            ("codec", encoded),
            ("binding", stored),
            ("analysis", analyzed),
            ("prepared", prepared),
        ] {
            let expected = (
                case["sqlstate"].as_str(),
                case["message"].as_str().unwrap(),
                case["detail"].as_str(),
                case["hint"].as_str(),
            );
            match result {
                Err(error)
                    if (
                        error.sqlstate(),
                        error.to_string().as_str(),
                        error.detail(),
                        error.hint(),
                    ) == expected => {}
                result => failures.push(format!(
                    "{owner}: {base}, {check}: {result:?}; expected {expected:?}"
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn named_row_fields_infer_prepared_parameter_types_like_postgresql() {
    let fixture: Fixture = serde_json::from_str(include_str!("../../named_rows.json")).unwrap();
    let catalog = Catalog(&fixture.states[0].catalog, true);
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../row_parameters.json")).unwrap();
    for case in cases {
        let sql = case["sql"].as_str().unwrap();
        let expected = case["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| Some(ColumnType::from_sql_name(name.as_str().unwrap()).unwrap()))
            .collect::<Vec<_>>();
        let declared = vec![None; expected.len()];
        let context = crate::binding::fixture::empty_binding_context();
        let mut plan = crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        assert_eq!(
            crate::binding::infer_prepared_parameter_types(&catalog, &plan, &declared, &context)
                .unwrap(),
            expected,
            "analysis: {sql}"
        );
        assert_eq!(
            crate::binding::read_prepared_inputs(&catalog, &mut plan, &declared, &context, None)
                .unwrap()
                .parameter_types,
            expected,
            "prepared: {sql}"
        );
    }
}
