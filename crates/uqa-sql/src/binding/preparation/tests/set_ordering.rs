//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` set-operation output-reference and diagnostic controls.

use super::*;

fn analyze(sql: &str, parameters: &[Option<ColumnType>]) -> Result<(), SQLError> {
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    infer_prepared_parameter_types(&NoRoutines, &plan, parameters, &assignment_context()).map(drop)
}

#[test]
fn query_ordering_preserves_ambiguous_output_diagnostics() {
    let error = analyze("SELECT 1 AS a,2 AS a ORDER BY a", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
    assert_eq!(error.to_string(), "ORDER BY \"a\" is ambiguous");
    let error = analyze(
        "SELECT q.a FROM (VALUES(1))q(a) CROSS JOIN (VALUES(2))s(a) ORDER BY a+1",
        &[],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
    assert_eq!(error.to_string(), "column reference \"a\" is ambiguous");
}

#[test]
fn set_ordering_accepts_output_names_positions_and_outer_expressions() {
    for operation in [
        "UNION",
        "UNION ALL",
        "INTERSECT",
        "INTERSECT ALL",
        "EXCEPT",
        "EXCEPT ALL",
    ] {
        for key in ["a", "(a)", "1"] {
            let sql = format!("SELECT 1 AS a {operation} SELECT 2 ORDER BY {key} DESC");
            analyze(&sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }
    for sql in [
        "SELECT * FROM (SELECT 1 AS a UNION SELECT 2) q ORDER BY a+1",
        "(SELECT a FROM (VALUES (1),(2)) t(a) ORDER BY a+1 LIMIT 1) UNION SELECT 3 ORDER BY 1",
    ] {
        analyze(sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn set_ordering_rejects_computed_targets_after_complete_clause_analysis() {
    for operation in [
        "UNION",
        "UNION ALL",
        "INTERSECT",
        "INTERSECT ALL",
        "EXCEPT",
        "EXCEPT ALL",
    ] {
        for key in ["a+1", "+a", "a::bigint", "1+1", "$1", "(SELECT 1)"] {
            let sql = format!("SELECT 1 AS a {operation} SELECT 2 ORDER BY {key}");
            let error = analyze(&sql, &[Some(ColumnType::Integer)]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
            assert_eq!(
                error.to_string(),
                "invalid UNION/INTERSECT/EXCEPT ORDER BY clause"
            );
            assert_eq!(
                error.detail(),
                Some("Only result column names can be used, not expressions or functions.")
            );
            assert_eq!(error.hint(), Some("Add the expression/function to every SELECT, or move the UNION into a FROM clause."));
        }
    }
    for (sql, state) in [
        (
            "SELECT 1 AS a UNION SELECT 2 ORDER BY a+1, missing",
            "42703",
        ),
        (
            "SELECT 1 AS a UNION SELECT 2 ORDER BY a+1 LIMIT missing",
            "0A000",
        ),
        (
            "SELECT 1 AS a UNION ALL SELECT 2 ORDER BY '{}'::json",
            "42883",
        ),
        ("SELECT missing UNION SELECT 2 ORDER BY 'x'", "42703"),
    ] {
        let error = analyze(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
}

#[test]
fn set_ordering_preserves_literal_and_duplicate_output_diagnostics() {
    for key in ["NULL", "false", "'a'", "1.0", "2147483648", "-2147483648"] {
        let sql = format!("SELECT 1 AS a UNION SELECT 2 ORDER BY {key}");
        let error = analyze(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42601"), "{sql}: {error}");
        assert_eq!(error.to_string(), "non-integer constant in ORDER BY");
    }
    for position in [0, -1, 2, -2_147_483_647, 2_147_483_647] {
        let sql = format!("SELECT 1 AS a UNION SELECT 2 ORDER BY {position}");
        let error = analyze(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P10"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            format!("ORDER BY position {position} is not in select list")
        );
    }
    for second in [1, 2] {
        let sql = format!("SELECT 1 AS a,{second} AS a UNION SELECT 3,4 ORDER BY a");
        let error = analyze(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42702"));
        assert_eq!(error.to_string(), "ORDER BY \"a\" is ambiguous");
    }
}

#[test]
fn set_ordering_retains_leaf_names_without_exposing_nested_query_outputs() {
    for (sql, table) in [
        ("SELECT 1 AS a UNION SELECT 2 AS b ORDER BY b+1", Some(2)),
        (
            "SELECT 1 AS a UNION SELECT 2 AS b UNION SELECT 3 AS c ORDER BY c",
            Some(3),
        ),
        (
            "SELECT 1 AS a UNION (SELECT 2 AS b UNION SELECT 3 AS c) ORDER BY c",
            Some(3),
        ),
        (
            "(SELECT 1 AS a UNION SELECT 2 AS b ORDER BY 1) UNION SELECT 3 AS c ORDER BY c",
            Some(2),
        ),
        (
            "SELECT 1 AS a UNION (SELECT 2 AS b UNION SELECT 3 AS c ORDER BY 1) ORDER BY c",
            None,
        ),
    ] {
        let error = analyze(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
        match table {
            Some(table) => assert!(
                error
                    .detail()
                    .unwrap()
                    .contains(&format!("\"*SELECT* {table}\"")),
                "{sql}: {error:?}"
            ),
            None => assert_eq!(error.detail(), None, "{sql}: {error:?}"),
        }
    }
    let error = analyze(
        "SELECT 1 AS a UNION SELECT 2 AS b UNION SELECT 3 AS b ORDER BY b",
        &[],
    )
    .unwrap_err();
    assert_eq!(error.detail(), Some("There are columns named \"b\", but they are in tables that cannot be referenced from this part of the query."));
    assert_eq!(error.hint(), Some("Try using a table-qualified name."));
}
