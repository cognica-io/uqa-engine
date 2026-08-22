//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock clause validation, merging, and outer-join reduction compilation.

use super::*;

#[test]
fn for_update_compiles_with_postgresql_lock_options() {
    let Statement::Select(select) = first("SELECT * FROM employees e FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(select.locking.len(), 1);
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForUpdate
    );
    assert_eq!(select.locking[0].wait, crate::ast::LockWait::NoWait);
    assert_eq!(select.locking[0].relations, ["e"]);

    let Statement::Select(select) = first("SELECT * FROM employees e FOR SHARE OF e SKIP LOCKED")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForShare
    );
    assert_eq!(select.locking[0].wait, crate::ast::LockWait::SkipLocked);

    let Statement::Select(select) = first("SELECT * FROM employees FOR NO KEY UPDATE") else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForNoKeyUpdate
    );
    assert!(select.locking[0].relations.is_empty());

    let Statement::Select(select) = first("SELECT * FROM employees FOR KEY SHARE") else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForKeyShare
    );

    let Statement::Select(select) =
        first("SELECT * FROM (SELECT id FROM employees FOR SHARE) AS e FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    let Some(crate::ast::FromClause::Subquery { body, .. }) = select.from else {
        panic!("expected derived table");
    };
    assert_eq!(body.locking.len(), 2);
    assert!(body.locking.iter().any(|clause| {
        clause.strength == crate::ast::LockStrength::ForShare
            && clause.wait == crate::ast::LockWait::Block
    }));
    assert!(body.locking.iter().any(|clause| {
        clause.strength == crate::ast::LockStrength::ForUpdate
            && clause.wait == crate::ast::LockWait::NoWait
            && clause.relations.is_empty()
    }));
}

#[test]
fn for_update_rejects_postgresql_illegal_shapes() {
    for (sql, expected) in [
        (
            "SELECT DISTINCT id FROM employees FOR UPDATE",
            "DISTINCT clause",
        ),
        (
            "SELECT department, count(*) FROM employees GROUP BY department FOR UPDATE",
            "GROUP BY clause",
        ),
        (
            "SELECT count(*) FROM employees FOR UPDATE",
            "aggregate functions",
        ),
        (
            "SELECT row_number() OVER (ORDER BY id) FROM employees FOR UPDATE",
            "window functions",
        ),
        (
            "SELECT id FROM employees ORDER BY row_number() OVER () FOR UPDATE",
            "window functions",
        ),
        (
            "SELECT id FROM employees UNION SELECT id FROM employees FOR UPDATE",
            "UNION/INTERSECT/EXCEPT",
        ),
        ("VALUES (1) FOR UPDATE", "VALUES"),
        (
            "SELECT * FROM generate_series(1, 3) AS g FOR UPDATE OF g",
            "function",
        ),
        (
            "SELECT * FROM employees e LEFT JOIN departments d ON e.department_id = d.id FOR UPDATE",
            "nullable side",
        ),
        (
            "SELECT count(*) FROM employees HAVING count(*) > 0 FOR UPDATE",
            "HAVING clause",
        ),
        (
            "SELECT * FROM (SELECT DISTINCT id FROM employees) AS e FOR UPDATE OF e",
            "DISTINCT clause",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}");
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql}: {error}"
        );
    }
    let missing = compile("SELECT * FROM employees e FOR UPDATE OF missing").unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42P01"));
    let hidden = compile("SELECT * FROM employees e FOR UPDATE OF employees").unwrap_err();
    assert_eq!(hidden.sqlstate(), Some("42P01"));
    let hidden_function =
        compile("SELECT * FROM generate_series(1, 3) AS g FOR UPDATE OF generate_series")
            .unwrap_err();
    assert_eq!(hidden_function.sqlstate(), Some("42P01"));
    let cte =
        compile("WITH e AS (SELECT * FROM employees) SELECT * FROM e FOR UPDATE OF e").unwrap_err();
    assert_eq!(cte.sqlstate(), Some("0A000"));
    assert!(cte.to_string().contains("WITH query"));

    for sql in [
        "SELECT 1 FOR UPDATE",
        "SELECT * FROM generate_series(1, 3) AS g FOR UPDATE",
        "WITH e AS (SELECT * FROM employees) SELECT * FROM e FOR UPDATE",
        "SELECT * FROM (VALUES (1)) AS v(id) FOR UPDATE OF v",
        "SELECT * FROM (WITH e AS (SELECT * FROM employees) SELECT * FROM e) AS s FOR UPDATE OF s",
    ] {
        compile(sql).unwrap_or_else(|error| panic!("unexpected error for {sql}: {error}"));
    }
}

#[test]
fn repeated_lock_targets_and_clauses_compile_for_executor_merging() {
    let Statement::Select(select) =
        first("SELECT * FROM employees e FOR KEY SHARE OF e, e SKIP LOCKED FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(select.locking.len(), 2);
    assert_eq!(select.locking[0].relations, ["e", "e"]);
    assert_eq!(
        select.locking[1].strength,
        crate::ast::LockStrength::ForUpdate
    );
    assert_eq!(select.locking[1].wait, crate::ast::LockWait::NoWait);
}

#[test]
fn row_lock_outer_join_reduction_uses_known_function_strictness() {
    compile("SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE abs(b.id) > 0 FOR UPDATE OF b")
        .expect("ABS is strict and rejects the null-extended side");
    let non_strict = compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE coalesce(b.id, 1) > 0 FOR UPDATE OF b",
    )
    .unwrap_err();
    assert!(
        non_strict.to_string().contains("nullable side"),
        "{non_strict}"
    );
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE application_strict(b.id) > 0 FOR UPDATE OF b",
    )
    .expect("catalog-owned function strictness is deferred until engine binding");
}

#[test]
fn row_lock_outer_join_reduction_preserves_between_three_valued_logic() {
    let not_between = compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id NOT BETWEEN b.low AND 10 FOR UPDATE OF b",
    )
    .unwrap_err();
    assert!(
        not_between.to_string().contains("nullable side"),
        "{not_between}"
    );
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id BETWEEN 0 AND b.high FOR UPDATE OF b",
    )
    .expect("a nullable upper bound makes BETWEEN NULL or FALSE, never TRUE");
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id BETWEEN SYMMETRIC 0 AND b.high FOR UPDATE OF b",
    )
    .expect("BETWEEN SYMMETRIC is strict in all three arguments");
}

#[test]
fn set_operation_locking_error_names_the_requested_strength() {
    let error =
        compile("SELECT id FROM employees UNION SELECT id FROM employees FOR SHARE").unwrap_err();
    assert!(error.to_string().contains("FOR SHARE"), "{error}");

    for sql in [
        "(SELECT id FROM employees FOR UPDATE) UNION SELECT id FROM employees",
        "SELECT id FROM employees UNION (SELECT id FROM employees FOR KEY SHARE)",
    ] {
        let error = compile(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        assert!(
            error.to_string().contains("UNION/INTERSECT/EXCEPT"),
            "{sql}: {error}"
        );
    }

    compile(
        "SELECT id FROM (SELECT id FROM employees FOR UPDATE) AS locked UNION SELECT id FROM employees",
    )
    .expect("locking inside a derived table is not a direct set-operation operand lock");
}
