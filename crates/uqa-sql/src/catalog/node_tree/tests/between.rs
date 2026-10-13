//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured comparison expansion and diagnostics.

use super::{expressions, Routines};
use crate::{ColumnType, RowSchema};

#[test]
fn between_catalog_nodes_and_sql_match_postgresql() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("between.json")).unwrap();
    let accepted = cases
        .iter()
        .filter(|case| case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 12);
    super::check_typed_nodes(&serde_json::to_string(&accepted).unwrap());
    let rejected = cases
        .iter()
        .filter(|case| !case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 1);
    for case in rejected {
        let base = ColumnType::from_sql_name(case["base"].as_str().unwrap()).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: None,
            routines: &Routines,
        };
        let statement = format!(
            "CREATE DOMAIN d AS {} CHECK ({})",
            case["base"].as_str().unwrap(),
            case["check"].as_str().unwrap()
        );
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let error = context.check(&domain.checks[0].expression).unwrap_err();
        assert_eq!(error.sqlstate(), case["error"]["sqlstate"].as_str());
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap()
        );
        assert_eq!(error.detail(), case["error"]["detail"].as_str());
        assert_eq!(error.hint(), case["error"]["hint"].as_str());
    }
}
