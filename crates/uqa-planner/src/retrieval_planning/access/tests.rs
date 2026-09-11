//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::retrieval_planning::testing::Inputs;
use uqa_core::{Predicate, Value};

#[test]
fn unsupported_or_failed_value_index_probe_stops_before_later_filters() {
    let expression = ScalarExpr::Literal(Value::Bool(true));
    let tree = OperatorTree::Intersect(
        ["first", "later"]
            .into_iter()
            .map(|field| OperatorTree::Filter {
                field: field.into(),
                predicate: Predicate::Equals(Value::Int(1)),
                source: None,
            })
            .collect(),
    );
    for support in [Ok(false), Err("index unavailable")] {
        let inputs = Inputs::new(support);
        let result = supports_acceleration(&inputs, "docs", &expression, &tree);
        match support {
            Ok(_) => assert!(!result.unwrap()),
            Err(_) => assert!(matches!(result, Err(SQLError::Internal(message))
                if message == "execute prepare value index: index unavailable")),
        }
        assert_eq!(*inputs.probes.borrow(), ["first"]);
    }
}

#[test]
fn selected_index_and_retrieval_paths_skip_fallback_value_index_probes() {
    let inputs = Inputs::new(Err("must not probe"));
    let index = OperatorTree::IndexScan {
        index_name: "docs_body".into(),
        field: "body".into(),
        predicate: Predicate::Equals(Value::Str("rust".into())),
    };
    assert!(supports_acceleration(
        &inputs,
        "docs",
        &ScalarExpr::Literal(Value::Bool(true)),
        &index,
    )
    .unwrap());
    let uqa_sql::ast::Statement::Select(statement) =
        uqa_sql::compile("SELECT * FROM docs WHERE text_match(body, 'rust')")
            .unwrap()
            .remove(0)
    else {
        panic!("expected a retrieval predicate")
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(statement.r#where.unwrap()).scalar;
    let retrieval = OperatorTree::Term {
        query: "rust".into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    };
    assert!(supports_acceleration(&inputs, "docs", &expression, &retrieval).unwrap());
    assert!(inputs.probes.borrow().is_empty());
}
