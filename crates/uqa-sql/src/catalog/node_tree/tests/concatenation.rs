//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured `PostgreSQL` concatenation signatures and typed catalog expressions.

use super::{expressions, Routines};
use crate::{ColumnType, RowSchema};

#[test]
fn concatenation_keeps_domain_enum_and_composite_argument_identity() {
    #[derive(serde::Deserialize)]
    struct Fixture {
        catalog: Vec<super::catalog::Type>,
        cases: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        check: String,
        conbin: String,
        sql: String,
        pretty: String,
    }
    let fixture: Fixture =
        serde_json::from_str(include_str!("concatenation_domains.json")).unwrap();
    let catalog = super::catalog::Catalog(&fixture.catalog, true);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)]);
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&ColumnType::Text),
        types: Some(&catalog),
        routines: &Routines,
    };
    assert_eq!(fixture.cases.len(), 11);
    for case in fixture.cases {
        let statement = format!("CREATE DOMAIN d AS text CHECK ({})", case.check);
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let actual = super::Field::Node(context.check(&domain.checks[0].expression).unwrap());
        assert_eq!(
            actual,
            super::parse(&case.conbin).unwrap(),
            "{}",
            case.check
        );
        for (pretty, expected) in [(false, case.sql), (true, case.pretty)] {
            assert_eq!(
                super::deparse::expression(&actual, &catalog, pretty).unwrap(),
                expected,
                "{}",
                case.check
            );
        }
    }
}

#[test]
fn concatenation_nodes_and_both_sql_forms_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("concatenation.json")).unwrap();
    let accepted = cases
        .into_iter()
        .filter(|case| case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 26);
    super::check_typed_nodes(&serde_json::to_string(&accepted).unwrap());
}

#[test]
fn invalid_concatenation_catalog_signatures_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("concatenation.json")).unwrap();
    let schema = RowSchema::default();
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&ColumnType::Text),
        types: None,
        routines: &Routines,
    };
    let errors = cases
        .into_iter()
        .filter(|case| !case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 3);
    for case in errors {
        let statement = format!(
            "CREATE DOMAIN d AS text CHECK ({})",
            case["check"].as_str().unwrap()
        );
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let error = context.check(&domain.checks[0].expression).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            case["error"]["sqlstate"].as_str(),
            "{statement}: {error}"
        );
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{statement}"
        );
        assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{statement}");
    }
}
