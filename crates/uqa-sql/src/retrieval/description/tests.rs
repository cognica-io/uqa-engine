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

#[test]
fn controlled_vector_description_admits_arguments_before_copying_and_keeps_resource_failures() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let expression = call(
        "knn_match",
        vec![
            ScalarExpr::Column("embedding".into()),
            ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Float(1.0)); 256 * 1024]),
            ScalarExpr::Literal(Value::Int(1)),
        ],
    );
    let budget = MemoryBudget::new(32 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let evaluations = std::cell::Cell::new(0);
    let allocations = allocation_counter::measure(|| {
        let result = describe_vector_call_with_control(
            &expression,
            &[],
            &|expr, _, control| {
                evaluations.set(evaluations.get() + 1);
                let ScalarExpr::Literal(value) = expr else {
                    panic!("constant expected")
                };
                Ok(control.copy_value(value)?)
            },
            &|_| true,
            &control,
        );
        let Err(error) = result else {
            panic!("resource failure became a deferred argument")
        };
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(evaluations.get() < 16 * 1024);
    assert!(allocations.bytes_max < 128 * 1024, "{allocations:?}");
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_description_propagates_both_cancellation_owners_inside_constant_evaluation() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let expression = call(
        "calibrated_vector_match",
        vec![
            ScalarExpr::Literal(Value::Str("embedding".into())),
            ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Float(1.0)); 128]),
            ScalarExpr::Literal(Value::Int(1)),
        ],
    );
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(32 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let evaluations = std::cell::Cell::new(0);
        let result = describe_vector_call_with_control(
            &expression,
            &[],
            &|expr, _, control| {
                evaluations.set(evaluations.get() + 1);
                if evaluations.get() == 7 {
                    if cancel_original {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                }
                let ScalarExpr::Literal(value) = expr else {
                    panic!("constant expected")
                };
                Ok(control.copy_value(value)?)
            },
            &|_| true,
            &control,
        );
        let Err(error) = result else {
            panic!("cancellation became a deferred argument")
        };
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(evaluations.get(), 7);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_description_preserves_invalid_and_deferred_arguments_and_retains_successful_inputs() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let expression = call(
        "knn_match",
        vec![
            ScalarExpr::QualifiedColumn {
                qualifier: "d".into(),
                column: "embedding".into(),
            },
            ScalarExpr::Array(vec![
                ScalarExpr::Literal(Value::Int(1)),
                ScalarExpr::Literal(Value::Int(0)),
            ]),
            ScalarExpr::Literal(Value::Int(3)),
        ],
    );
    let evaluate = |expr: &ScalarExpr, _: &[SQLParam], control: &ProductionControl<'_>| {
        let ScalarExpr::Literal(value) = expr else {
            panic!("literal expected")
        };
        Ok(control.copy_value(value)?)
    };
    let described =
        describe_vector_call_with_control(&expression, &[], &evaluate, &|_| true, &control)
            .unwrap()
            .unwrap();
    assert!(
        matches!(&**described.bound.as_ref().unwrap(), RetrievalExpr::KNN { query_vector, k: 3, .. } if query_vector == &[1.0, 0.0])
    );
    assert!(budget.used() >= 2 * "embedding".len() + 2 * size_of::<f32>());
    drop(described);
    assert_eq!(budget.used(), 0);
    let deferred = describe_vector_call_with_control(
        &expression,
        &[],
        &|_, _, _| panic!("deferred evaluation"),
        &|_| false,
        &control,
    )
    .unwrap()
    .unwrap();
    assert!(deferred.bound.is_none());
    assert!(!deferred.invalid_arguments);
    drop(deferred);
    let invalid = call(
        "knn_match",
        vec![
            ScalarExpr::Column("embedding".into()),
            ScalarExpr::Array(vec![]),
            ScalarExpr::Literal(Value::Int(1)),
        ],
    );
    let rejected = describe_vector_call_with_control(&invalid, &[], &evaluate, &|_| true, &control)
        .unwrap()
        .unwrap();
    assert!(rejected.invalid_arguments);
    drop(rejected);
    assert_eq!(budget.used(), 0);
}
