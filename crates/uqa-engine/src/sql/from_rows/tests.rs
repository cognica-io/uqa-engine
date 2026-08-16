//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn combine_filters_handles_empty_and_single_inputs_without_panicking() {
    assert!(combine_filters(Vec::<ScalarExpr>::new()).is_none());
    let combined = combine_filters([ScalarExpr::Literal(Value::Bool(true))]);
    assert!(matches!(
        combined,
        Some(ScalarExpr::Literal(Value::Bool(true)))
    ));
}

#[test]
fn engine_backed_projection_functions_reject_a_missing_engine_context() {
    let row = ResultRow::new();
    for function in [
        "deep_learn",
        "graph_create",
        "graph_drop",
        "create_graph",
        "drop_graph",
    ] {
        let mut evaluate = |_: &ScalarExpr| Ok(Value::Null);
        let error = engine_func_intercept(None, function, &[], &row, &mut evaluate)
            .expect_err("engine-backed functions must not report success without an engine");
        assert!(
            matches!(
                &error,
                SQLError::Unsupported(message)
                    if message == &format!("{function} requires an engine-backed projection")
            ),
            "unexpected {function} error: {error:?}"
        );
    }
}

#[test]
fn score_projection_uses_explicit_provenance_even_for_zero() {
    let args = [ScalarExpr::Literal(Value::Str("query".into()))];
    let mut evaluate = |expr: &ScalarExpr| match expr {
        ScalarExpr::Literal(value) => Ok(value.clone()),
        _ => Ok(Value::Null),
    };
    let scored_row = ResultRow::from([
        (super::super::SCORE_COLUMN.into(), Value::Float(99.0)),
        (SCORE_PROVENANCE_COLUMN.into(), Value::Float(0.0)),
    ]);
    assert_eq!(
        engine_func_intercept(None, "score_bm25", &args, &scored_row, &mut evaluate).unwrap(),
        Some(Value::Float(0.0))
    );

    let unscored_row = ResultRow::from([
        (super::super::SCORE_COLUMN.into(), Value::Float(0.0)),
        (SCORE_PROVENANCE_COLUMN.into(), Value::Null),
    ]);
    let error =
        engine_func_intercept(None, "score_bm25", &args, &unscored_row, &mut evaluate).unwrap_err();
    assert!(error.to_string().contains("score-bearing"), "{error}");
}

#[test]
fn qualified_score_projection_uses_structured_provenance_identity() {
    use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema};

    let row = OwnedPhysicalRow::new(
        RowSchema::with_identities(
            vec!["body".into(), SCORE_PROVENANCE_COLUMN.into()],
            vec![
                ColumnIdentity::qualified("hit", "body"),
                ColumnIdentity::qualified("hit", SCORE_PROVENANCE_COLUMN),
            ],
            vec![None, None],
        ),
        PhysicalRow::from_values(vec![Value::Str("rust".into()), Value::Float(0.25)]),
    );
    let args = [
        ScalarExpr::qualified_column("hit", "body"),
        ScalarExpr::Literal(Value::Str("rust".into())),
    ];
    let mut evaluate = |expr: &ScalarExpr| match expr {
        ScalarExpr::Literal(value) => Ok(value.clone()),
        _ => Ok(Value::Null),
    };
    assert_eq!(
        engine_func_intercept(None, "score_bm25", &args, &row, &mut evaluate).unwrap(),
        Some(Value::Float(0.25))
    );
}
