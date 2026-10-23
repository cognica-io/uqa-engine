//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn empty_derived_rows_bind_and_reject_excess_aliases_without_execution() {
    let context = crate::binding::fixture::empty_binding_context();
    for sql in [
        "SELECT q.* FROM (SELECT)q",
        "SELECT q.* FROM (VALUES(1))l(k) LEFT JOIN (SELECT)q ON FALSE",
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let schema = crate::binding::analyze_query_plan_schema(
            &EmptyRoutineResolution,
            &query,
            &[],
            &context,
            None,
        )
        .unwrap();
        assert!(schema.is_empty(), "{sql}");
    }
    for sql in [
        "SELECT q FROM (SELECT)q",
        "SELECT q FROM (VALUES(1))l(k) LEFT JOIN (SELECT)q ON FALSE",
        "WITH q AS (SELECT) SELECT q FROM q",
        "SELECT j FROM ((SELECT)l LEFT JOIN (SELECT)r ON FALSE)j",
        "SELECT (SELECT q) AS nested FROM (SELECT)q",
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let schema = crate::binding::analyze_query_plan_schema(
            &EmptyRoutineResolution,
            &query,
            &[],
            &context,
            None,
        )
        .unwrap();
        assert_eq!(schema.column_types(), &[Some(ColumnType::Record)], "{sql}");
    }
    for (sql, state, message) in [
        (
            "SELECT * FROM (SELECT)q(a)",
            "42P10",
            "table \"q\" has 0 columns available but 1 columns specified",
        ),
        (
            "SELECT q.missing FROM (SELECT)q",
            "42703",
            "column q.missing does not exist",
        ),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let error = crate::binding::analyze_query_plan_schema(
            &EmptyRoutineResolution,
            &query,
            &[],
            &context,
            None,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
}
