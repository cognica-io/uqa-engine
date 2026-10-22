//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bare_star_resolves_zero_arguments_before_rejecting_scalar_call_syntax() {
    for (sql, state, message) in [
        (
            "SELECT random(*)",
            "42809",
            "random(*) specified, but random is not an aggregate function",
        ),
        (
            "SELECT random(*) FILTER (WHERE true)",
            "42809",
            "random(*) specified, but random is not an aggregate function",
        ),
        (
            "SELECT absent_star_function(*)",
            "42883",
            "function absent_star_function() does not exist",
        ),
        (
            "SELECT pg_catalog.sum(*)",
            "42883",
            "function pg_catalog.sum() does not exist",
        ),
        (
            "SELECT pg_catalog.array_agg(*)",
            "42883",
            "function pg_catalog.array_agg() does not exist",
        ),
        (
            "SELECT pg_catalog.count()",
            "42809",
            "pg_catalog.count(*) must be used to call a parameterless aggregate function",
        ),
        (
            "SELECT pg_catalog.sum()",
            "42883",
            "function pg_catalog.sum() does not exist",
        ),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context())
            .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string()),
            (Some(state), message.into()),
            "{sql}"
        );
    }
}

#[test]
fn call_signature_keeps_whole_row_arguments_separate_from_bare_star() {
    let schema = crate::RowSchema::default();
    let signature =
        crate::function_call_argument_signature(&[ScalarExpr::Star], &schema, &[], None).unwrap();
    assert_eq!(signature, (Vec::new(), Vec::new(), false));
    let signature = crate::function_call_argument_signature(
        &[ScalarExpr::QualifiedStar("q".into())],
        &schema,
        &[],
        None,
    )
    .unwrap();
    assert_eq!(signature.0, [None]);
    assert_eq!(signature.1.len(), 1);
    assert!(!signature.2);
    for sql in [
        "SELECT count(*) FROM assignment_target",
        "SELECT pg_catalog.count(*) FROM assignment_target",
        "SELECT count(assignment_target.*) FROM assignment_target",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context())
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}
