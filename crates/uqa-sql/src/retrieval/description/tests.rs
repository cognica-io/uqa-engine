//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

fn call(name: &str, args: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: None,
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

#[test]
fn diskann_explain_description_keeps_field_identity_without_evaluating_deferred_values() {
    let expression = call(
        "knn_match",
        vec![
            ScalarExpr::QualifiedColumn {
                qualifier: "d".into(),
                column: "embedding".into(),
            },
            call("volatile_vector", vec![]),
            ScalarExpr::Literal(Value::Int(3)),
        ],
    );
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|_, _| panic!("deferred arguments must not run"),
        stores: &|_: &str| false,
    };
    let description = describe_vector_call(&expression, &constants, &|_| false)
        .unwrap()
        .unwrap();
    assert_eq!(description.field.as_deref(), Some("embedding"));
    assert_eq!(description.qualifier.as_deref(), Some("d"));
    assert!(description.bound.is_none());
    assert!(!description.invalid_arguments);
    let invalid = call("knn_match", vec![call("volatile_field", vec![])]);
    assert!(matches!(
        describe_vector_call(&invalid, &constants, &|_| false),
        Err(SQLError::BadArity { .. })
    ));
}

#[test]
fn diskann_explain_description_reuses_the_checked_vector_contract() {
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|expression, _| match expression {
            ScalarExpr::Literal(value) => Ok(value.clone()),
            _ => Err(SQLError::Internal(
                "fixture contains a nonliteral value".into(),
            )),
        },
        stores: &|_: &str| false,
    };
    let args = vec![
        ScalarExpr::Column("embedding".into()),
        ScalarExpr::Array(vec![
            ScalarExpr::Literal(Value::Int(1)),
            ScalarExpr::Literal(Value::Int(0)),
        ]),
        ScalarExpr::Literal(Value::Int(3)),
    ];
    let expression = call("knn_match", args.clone());
    let description = describe_vector_call(&expression, &constants, &|_| true)
        .unwrap()
        .unwrap();
    assert!(
        matches!(description.bound, Some(RetrievalExpr::KNN { query_vector, k: 3, .. }) if query_vector == [1.0, 0.0])
    );
    let mut invalid = args;
    invalid[2] = ScalarExpr::Literal(Value::Int(0));
    let description = describe_vector_call(&call("knn_match", invalid), &constants, &|_| true)
        .unwrap()
        .unwrap();
    assert!(
        description.bound.is_none(),
        "a rejected SQL K must not become an executable diagnostic query"
    );
    assert!(description.invalid_arguments);
}
