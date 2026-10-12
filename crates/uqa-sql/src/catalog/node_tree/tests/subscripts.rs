//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent typed array endpoint, type and collation observations.

use super::catalog::{Catalog, Type};
use super::{deparse, expressions, parse, Field, Node, Routines};
use crate::{ast::Expr, RowSchema};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    catalog: Vec<Type>,
    cases: Vec<Case>,
    errors: Vec<ErrorCase>,
}

#[derive(Deserialize)]
struct Case {
    base: String,
    check: String,
    conbin: String,
    sql: String,
    pretty: String,
}

#[derive(Deserialize)]
struct ErrorCase {
    base: String,
    check: String,
    error: Diagnostic,
}

#[derive(Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
}

fn expression(base: &str, check: &str) -> Expr {
    let sql = format!("CREATE DOMAIN test_domain AS {base} CHECK ({check})");
    let crate::Statement::CreateDomain(mut domain) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("domain declaration");
    };
    domain.checks.remove(0).expression
}

#[test]
fn typed_array_access_nodes_and_sql_match_postgresql() {
    let fixture: Fixture = serde_json::from_str(include_str!("subscripts.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    let mut failures = Vec::new();
    for case in &fixture.cases {
        let base = catalog.named(&case.base).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&catalog),
            routines: &Routines,
        };
        let expected = parse(&case.conbin).unwrap();
        match context.check(&expression(&case.base, &case.check)) {
            Ok(node) => {
                let actual = Field::Node(node);
                if actual != expected {
                    failures.push(format!(
                        "{} {}: actual {actual}\nexpected {expected}",
                        case.base, case.check
                    ));
                }
                for (pretty, sql) in [(false, &case.sql), (true, &case.pretty)] {
                    match deparse::expression(&actual, &catalog, pretty) {
                        Ok(output) if &output == sql => {}
                        result => failures.push(format!(
                            "{} {} pretty={pretty}: {result:?}; expected {sql}",
                            case.base, case.check
                        )),
                    }
                }
            }
            Err(error) => failures.push(format!("{} {}: {error}", case.base, case.check)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn typed_array_access_preserves_postgresql_analysis_errors() {
    let fixture: Fixture = serde_json::from_str(include_str!("subscripts.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    for case in fixture.errors {
        let base = catalog.named(&case.base).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&catalog),
            routines: &Routines,
        };
        let error = context
            .check(&expression(&case.base, &case.check))
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(case.error.sqlstate.as_str()),
            "{}: {error}",
            case.check
        );
        assert_eq!(error.to_string(), case.error.message, "{}", case.check);
    }
}

fn replace(node: &mut Node, name: &str, value: Field) {
    node.fields
        .iter_mut()
        .find(|(field, _)| field == name)
        .unwrap()
        .1 = value;
}

#[test]
fn typed_array_reconstruction_rejects_ambiguous_bounds_and_result_identity() {
    let fixture: Fixture = serde_json::from_str(include_str!("subscripts.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    let Field::Node(parent) = parse(&fixture.cases[0].conbin).unwrap() else {
        panic!("NULL test")
    };
    let Field::Node(array) = parent.field("arg").unwrap() else {
        panic!("array read")
    };
    for (name, value) in [
        ("refupperindexpr", Field::List(vec![])),
        ("refupperindexpr", Field::List(vec![Field::Null])),
        ("refupperindexpr", Field::List(vec![Field::List(vec![])])),
        ("refupperindexpr", Field::List(vec![Field::Null; 7])),
        (
            "reflowerindexpr",
            Field::List(vec![Field::Null, Field::Null]),
        ),
        ("refcontainertype", 0.into()),
        ("refelemtype", (-1).into()),
        ("refrestype", 1007.into()),
        ("refexpr", Field::List(vec![Field::Null])),
        ("refassgnexpr", array.field("refexpr").unwrap().clone()),
    ] {
        let mut node = array.clone();
        replace(&mut node, name, value);
        for pretty in [false, true] {
            assert!(
                deparse::expression(&node.clone().into(), &catalog, pretty).is_err(),
                "{name}"
            );
        }
    }
}
