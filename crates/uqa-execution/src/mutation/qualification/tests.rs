//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

#[test]
fn retrieval_support_spills_without_using_scores_as_truth_values() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let entries = (0..4096)
        .rev()
        .chain([0, 4095])
        .map(|doc_id| ScoredEntry {
            doc_id,
            score: if doc_id % 2 == 0 { 0.0 } else { -1.0 },
        })
        .collect();
    let rows = retain_support(entries, &control).unwrap();
    for id in [0, 1, 2048, 4095] {
        assert!(contains(&rows, id, &control).unwrap());
    }
    assert!(!contains(&rows, 4096, &control).unwrap());
    assert!(!contains(&rows, u64::MAX, &control).unwrap());
    drop(rows);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn cancelled_support_retention_releases_its_workspace() {
    let control = StorageReadControl::with_limit(64 * 1024);
    control.cancellation().cancel();
    let error = match retain_support(
        vec![ScoredEntry {
            doc_id: 1,
            score: 0.0,
        }],
        &control,
    ) {
        Ok(_) => panic!("cancelled retention succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retrieval_qualification_keeps_lazy_boolean_evaluation_and_field_arguments() {
    let calls = std::cell::Cell::new(0);
    let expression = ScalarExpr::Func {
        name: "text_match".into(),
        binding: None,
        args: vec![
            ScalarExpr::Column("body".into()),
            ScalarExpr::Literal(Value::Str("alpha".into())),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let retrieval = |name: &str, args: &[ScalarExpr]| {
        assert_eq!(name, "text_match");
        assert_eq!(args[0], ScalarExpr::Column("body".into()));
        calls.set(calls.get() + 1);
        Ok(true)
    };
    let context = crate::ScalarEvalContext::new(None, &[]).with_retrieval_predicate(&retrieval);
    for (predicate, expected) in [
        (
            ScalarExpr::And(vec![
                ScalarExpr::Literal(Value::Bool(false)),
                expression.clone(),
            ]),
            false,
        ),
        (
            ScalarExpr::Or(vec![
                ScalarExpr::Literal(Value::Bool(true)),
                expression.clone(),
            ]),
            true,
        ),
    ] {
        assert_eq!(
            crate::eval_scalar(&predicate, &context).unwrap(),
            Value::Bool(expected)
        );
        assert_eq!(calls.get(), 0);
    }
    assert_eq!(
        crate::eval_scalar(&expression, &context).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(calls.get(), 1);
}
