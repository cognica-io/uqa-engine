//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named ROW nodes retain source positions while output reads current descriptors.

use super::super::{
    catalog::{Catalog, Type},
    deparse, expressions, parse, Field, Routines,
};
use crate::{ColumnType, RowSchema, Statement};
use serde::Deserialize;

mod errors;
mod stored;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
    states: Vec<State>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    base: String,
    check: String,
}

#[derive(Deserialize)]
struct State {
    name: String,
    catalog: Vec<Type>,
    observations: Vec<Observation>,
}

#[derive(Deserialize)]
struct Observation {
    name: String,
    conbin: String,
    sql: String,
    pretty: String,
}

#[test]
fn named_rows_match_postgresql_through_descriptor_changes() {
    let fixture: Fixture = serde_json::from_str(include_str!("../named_rows.json")).unwrap();
    let initial = Catalog(&fixture.states[0].catalog, true);
    let mut failures = Vec::new();
    for case in fixture.cases {
        let base = ColumnType::from_sql_name(&case.base).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let Statement::CreateDomain(domain) = crate::compile(&format!(
            "CREATE DOMAIN d AS {} CHECK({})",
            case.base, case.check
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain declaration")
        };
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&initial),
            routines: &Routines,
        };
        let node = match context.check(&domain.checks[0].expression) {
            Ok(node) => Field::Node(node),
            Err(error) => {
                failures.push(format!("{}: {error}", case.check));
                continue;
            }
        };
        assert_eq!(parse(&node.to_string()).unwrap(), node);
        for state in &fixture.states {
            let expected = state
                .observations
                .iter()
                .find(|observation| observation.name == case.name)
                .unwrap();
            if node != parse(&expected.conbin).unwrap() {
                failures.push(format!(
                    "{} in {}: node {node}; expected {}",
                    case.check, state.name, expected.conbin
                ));
            }
            let catalog = Catalog(&state.catalog, false);
            for (pretty, expected) in [(false, &expected.sql), (true, &expected.pretty)] {
                let result = deparse::expression(&node, &catalog, pretty);
                if result.as_ref().ok() != Some(expected) {
                    failures.push(format!(
                        "{} in {} (pretty={pretty}): {result:?}; expected {expected}",
                        case.check, state.name
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
