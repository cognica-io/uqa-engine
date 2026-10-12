//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(serde::Deserialize)]
struct DiagnosticCase {
    sql: String,
    error: Option<serde_json::Value>,
}

#[test]
fn rule_event_namespaces_match_postgresql_diagnostics() {
    let catalog = collated_catalog();
    let context = catalog.context();
    let columns = catalog.columns.as_ref().unwrap();
    let mut cases: Vec<DiagnosticCase> =
        serde_json::from_str(include_str!("diagnostics.json")).unwrap();
    cases.extend(
        serde_json::from_str::<Vec<DiagnosticCase>>(include_str!("parenthesized_diagnostics.json"))
            .unwrap(),
    );
    let mut mismatches = Vec::new();
    for case in cases {
        let Statement::CreateRule(rule) = crate::compile(&case.sql).unwrap().remove(0) else {
            panic!("rule")
        };
        let result = if let Some(mut condition) = rule.condition {
            context
                .validate_rule_condition(&mut condition, columns, rule.event, None, None)
                .map(drop)
        } else {
            let mut action = rule.actions.into_iter().next().unwrap();
            context
                .validate_rule_action_definition(
                    &mut action,
                    columns,
                    rule.event,
                    RelationLookupMode::Dynamic,
                )
                .map(drop)
        };
        match (result, case.error) {
            (Ok(()), None) => {}
            (Err(actual), Some(expected)) => {
                if actual.sqlstate() != expected["sqlstate"].as_str()
                    || actual.to_string() != expected["message"].as_str().unwrap()
                    || actual.detail() != expected["detail"].as_str()
                    || actual.hint() != expected["hint"].as_str()
                {
                    mismatches.push(format!(
                        "{}: actual {actual:?}; expected {expected}",
                        case.sql
                    ));
                }
            }
            (Ok(()), Some(expected)) => mismatches.push(format!(
                "{}: unexpectedly accepted; expected {expected}",
                case.sql
            )),
            (Err(actual), None) => mismatches.push(format!("{}: unexpected {actual:?}", case.sql)),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
