//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning calls are placed and checked while statements are analyzed, before planning can fold or move their expressions.

use super::*;

const HINT: &str = "You might be able to move the set-returning function into a LATERAL FROM item.";

fn analyze(sql: &str) -> Result<(), SQLError> {
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context()).map(drop)
}

fn assert_rejected(sql: &str, sqlstate: &str, message: &str, hint: Option<&str>) {
    let error = analyze(sql).expect_err(sql);
    assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
    assert_eq!(error.hint(), hint, "{sql}");
}

#[test]
fn set_returning_calls_are_rejected_where_postgresql_places_none() {
    for (sql, context) in [
        ("UPDATE assignment_target SET id = generate_series(1, 2)", "UPDATE"),
        ("UPDATE assignment_target SET value[generate_series(1, 2)] = 1", "UPDATE"),
        ("UPDATE assignment_target SET id = 1 WHERE generate_series(1, 2) > 0", "WHERE"),
        ("UPDATE assignment_target SET id = 1 RETURNING generate_series(1, 2)", "RETURNING"),
        ("DELETE FROM assignment_target WHERE regexp_matches('a', 'a') IS NOT NULL", "WHERE"),
        ("DELETE FROM assignment_target RETURNING regexp_matches('a', 'a')", "RETURNING"),
        ("INSERT INTO assignment_target (id) VALUES (1) RETURNING generate_series(1, 2)", "RETURNING"),
        ("INSERT INTO assignment_target (id) VALUES (generate_series(1, 2)), (3)", "VALUES"),
        ("INSERT INTO assignment_target (id) VALUES (1), (generate_series(1, 2))", "VALUES"),
        ("INSERT INTO assignment_target (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET id = generate_series(1, 2)", "UPDATE"),
        ("INSERT INTO assignment_target (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET id = 2 WHERE generate_series(1, 2) > 0", "WHERE"),
        ("INSERT INTO assignment_target (id) VALUES (1) ON CONFLICT ((generate_series(1, 2))) DO NOTHING", "index expressions"),
        ("INSERT INTO assignment_target (id) VALUES (1) ON CONFLICT (id) WHERE generate_series(1, 2) > 0 DO NOTHING", "index predicates"),
        ("MERGE INTO assignment_target t USING (SELECT 1 AS k) s ON generate_series(1, 2) = s.k WHEN MATCHED THEN DO NOTHING", "JOIN conditions"),
        ("MERGE INTO assignment_target t USING (SELECT 1 AS k) s ON t.id = s.k WHEN MATCHED AND generate_series(1, 2) > 0 THEN DO NOTHING", "MERGE WHEN conditions"),
        ("MERGE INTO assignment_target t USING (SELECT 1 AS k) s ON t.id = s.k WHEN MATCHED THEN UPDATE SET id = generate_series(1, 2)", "UPDATE"),
        ("MERGE INTO assignment_target t USING (SELECT 1 AS k) s ON t.id = s.k WHEN MATCHED THEN DO NOTHING RETURNING generate_series(1, 2)", "RETURNING"),
        // Planning would fold these conditions; analysis sees them as written.
        ("SELECT 1 WHERE (generate_series(0, 1)::boolean) OR true", "WHERE"),
        ("SELECT 1 HAVING generate_series(1, 2) > 0", "HAVING"),
        ("SELECT 1 LIMIT generate_series(1, 2)", "LIMIT"),
        ("SELECT 1 OFFSET generate_series(1, 2)", "OFFSET"),
        ("SELECT 1 FROM (SELECT 1) a(x) JOIN (SELECT 1) b(y) ON generate_series(1, 2) > 0", "JOIN conditions"),
        ("SELECT count(*) FILTER (WHERE generate_series(1, 2) > 0)", "FILTER"),
        ("SELECT sum(generate_series(1, 2)) FILTER (WHERE generate_series(1, 2) > 0)", "FILTER"),
        ("SELECT row_number() OVER (ROWS generate_series(1, 2) PRECEDING)", "window definitions"),
        ("VALUES (generate_series(1, 2))", "VALUES"),
        ("SELECT * FROM (VALUES (generate_series(1, 2))) v(x)", "VALUES"),
    ] {
        assert_rejected(
            sql,
            "0A000",
            &format!("set-returning functions are not allowed in {context}"),
            None,
        );
    }
    for sql in [
        "SELECT * FROM generate_series(1, generate_series(1, 2))",
        "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(generate_series(1, 2), 3))",
    ] {
        assert_rejected(
            sql,
            "0A000",
            "set-returning functions must appear at top level of FROM",
            None,
        );
    }
}

#[test]
fn constructs_that_cannot_contain_a_set_report_postgresql_diagnostics() {
    for (sql, message) in [
        (
            "SELECT CASE WHEN true THEN generate_series(1, 2) END",
            "set-returning functions are not allowed in CASE",
        ),
        (
            "SELECT CASE generate_series(1, 2) WHEN 1 THEN 1 END",
            "set-returning functions are not allowed in CASE",
        ),
        (
            "SELECT CASE WHEN true THEN 1 ELSE generate_series(1, 2) END",
            "set-returning functions are not allowed in CASE",
        ),
        (
            "SELECT coalesce(generate_series(1, 2), 0)",
            "set-returning functions are not allowed in COALESCE",
        ),
        (
            "SELECT sum(generate_series(1, 2))",
            "aggregate function calls cannot contain set-returning function calls",
        ),
        (
            "SELECT array_agg(1 ORDER BY generate_series(1, 2))",
            "aggregate function calls cannot contain set-returning function calls",
        ),
        (
            "SELECT lag(generate_series(1, 2)) OVER ()",
            "window function calls cannot contain set-returning function calls",
        ),
    ] {
        assert_rejected(sql, "0A000", message, Some(HINT));
    }
    for (sql, message) in [
        (
            "SELECT NOT (generate_series(0, 1)::boolean)",
            "argument of NOT must not return a set",
        ),
        (
            "SELECT (generate_series(0, 1)::boolean) AND true",
            "argument of AND must not return a set",
        ),
        (
            "SELECT (generate_series(0, 1)::boolean) OR true",
            "argument of OR must not return a set",
        ),
        (
            "SELECT generate_series(1, 2) BETWEEN 1 AND 2",
            "argument of AND must not return a set",
        ),
        (
            "SELECT 1 BETWEEN generate_series(1, 2) AND 2",
            "argument of AND must not return a set",
        ),
        (
            "SELECT generate_series(1, 2) NOT BETWEEN 1 AND 1",
            "argument of OR must not return a set",
        ),
        (
            "SELECT generate_series(1, 2) BETWEEN SYMMETRIC 1 AND 2",
            "argument of AND must not return a set",
        ),
        (
            "SELECT CASE WHEN generate_series(0, 1)::boolean THEN 1 END",
            "argument of CASE/WHEN must not return a set",
        ),
        (
            "SELECT CASE 1 WHEN generate_series(1, 2) THEN 1 END",
            "argument of CASE/WHEN must not return a set",
        ),
        (
            "SELECT 1 IN (generate_series(1, 2))",
            "argument of IN must not return a set",
        ),
        (
            "SELECT generate_series(0, 1)::boolean IS TRUE",
            "argument of IS TRUE must not return a set",
        ),
        (
            "SELECT (generate_series(1, 2), 1) = (1, 1)",
            "row comparison operator must not return a set",
        ),
        (
            "SELECT generate_series(1, 2) IN (SELECT 1)",
            "row comparison operator must not return a set",
        ),
    ] {
        assert_rejected(sql, "42804", message, None);
    }
}

#[test]
fn set_returning_calls_expand_where_postgresql_processes_a_target_list() {
    for sql in [
        "SELECT generate_series(generate_series(1, 2), 3)",
        "SELECT 1 GROUP BY generate_series(1, 2)",
        "SELECT 1 ORDER BY generate_series(1, 2)",
        "SELECT DISTINCT ON (generate_series(1, 2)) 1",
        "SELECT row_number() OVER (PARTITION BY generate_series(1, 2) ORDER BY generate_series(1, 3))",
        "SELECT NOT (row_number() OVER (PARTITION BY generate_series(1, 2)) > 1)",
        "SELECT generate_series(1, 2) IN (1, 2)",
        "SELECT 1 IN (generate_series(1, 2), 3)",
        "SELECT nullif(generate_series(1, 2), 1)",
        "SELECT CASE WHEN (SELECT generate_series(1, 1)) = 1 THEN 1 END",
        "SELECT NOT EXISTS (SELECT generate_series(1, 2))",
        "SELECT * FROM generate_series(1, 2) g, LATERAL generate_series(g, 3)",
        "INSERT INTO assignment_target (id) VALUES (generate_series(1, 3))",
        "INSERT INTO assignment_target (id) SELECT generate_series(1, 3)",
        "UPDATE assignment_target SET id = (SELECT generate_series(1, 1))",
    ] {
        analyze(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}
