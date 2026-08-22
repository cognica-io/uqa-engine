//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, window, and fetch-clause compilation.

use super::*;

#[test]
fn select_with_function_call_and_order_by() {
    let stmt = first(
        "SELECT id, title, _score AS s FROM docs \
         WHERE text_match(body, 'rust language') \
         ORDER BY _score DESC LIMIT 5",
    );
    let Statement::Select(s) = stmt else {
        panic!("not SELECT");
    };
    assert_eq!(s.projections.len(), 3);
    assert_eq!(s.projections[2].alias.as_deref(), Some("s"));
    match &s.from {
        Some(FromClause::Table { name, .. }) => assert_eq!(name, "docs"),
        other => panic!("expected single-table FROM, got {other:?}"),
    }
    match &s.r#where {
        Some(Expr::Func {
            distinct: false,
            order_by,
            filter: None,
            ..
        }) if order_by.is_empty() => {}
        other => panic!("expected scalar function call, got {other:?}"),
    }
    assert_eq!(s.order_by.len(), 1);
    assert!(s.order_by[0].descending);
    match &s.limit {
        Some(Expr::Literal(uqa_core::Value::Int(5))) => {}
        other => panic!("expected LIMIT 5, got {other:?}"),
    }
}

#[test]
fn named_windows_resolve_inheritance_and_frames() {
    let Statement::Select(select) = first(
        "SELECT sum(x) OVER child FROM measurements \
         WINDOW base AS (PARTITION BY grp), \
         child AS (base ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
    ) else {
        panic!("not SELECT");
    };
    let Expr::WindowCall { spec, .. } = &select.projections[0].expr else {
        panic!("not a window call");
    };
    assert!(spec.reference.is_none());
    assert!(matches!(spec.partition_by.as_slice(), [Expr::Column(name)] if name == "grp"));
    assert!(matches!(
        spec.order_by.as_slice(),
        [OrderBy {
            expr: Expr::Column(name),
            descending: false,
            ..
        }] if name == "x"
    ));
    assert!(matches!(
        spec.frame,
        Some(crate::ast::WindowFrame {
            mode: crate::ast::FrameMode::Rows,
            start: crate::ast::FrameBound::UnboundedPreceding,
            end: crate::ast::FrameBound::CurrentRow,
        })
    ));

    let Statement::Select(direct) = first(
        "SELECT sum(x) OVER framed FROM measurements \
         WINDOW framed AS (ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
    ) else {
        panic!("not SELECT");
    };
    let Expr::WindowCall { spec, .. } = &direct.projections[0].expr else {
        panic!("not a window call");
    };
    assert!(spec.frame.is_some());
}

#[test]
fn named_window_definition_errors_match_postgresql() {
    for (sql, sqlstate, message) in [
        (
            "SELECT sum(x) OVER missing FROM measurements",
            "42704",
            "window \"missing\" does not exist",
        ),
        (
            "SELECT sum(x) OVER w FROM measurements WINDOW w AS (), w AS ()",
            "42P20",
            "window \"w\" is already defined",
        ),
        (
            "SELECT sum(x) OVER w2 FROM measurements WINDOW w2 AS (w), w AS ()",
            "42704",
            "window \"w\" does not exist",
        ),
        (
            "SELECT sum(x) OVER w2 FROM measurements WINDOW w AS (PARTITION BY x), w2 AS (w PARTITION BY x)",
            "42P20",
            "cannot override PARTITION BY clause",
        ),
        (
            "SELECT sum(x) OVER w2 FROM measurements WINDOW w AS (), w2 AS (w PARTITION BY x)",
            "42P20",
            "cannot override PARTITION BY clause",
        ),
        (
            "SELECT sum(x) OVER w2 FROM measurements WINDOW w AS (ORDER BY x), w2 AS (w ORDER BY x)",
            "42P20",
            "cannot override ORDER BY clause",
        ),
        (
            "SELECT sum(x) OVER (w) FROM measurements WINDOW w AS (ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
            "42P20",
            "cannot copy window \"w\" because it has a frame clause",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(sqlstate), "unexpected error: {error}");
        assert!(
            error.to_string().contains(message),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn fetch_with_ties_preserves_its_boundary_and_requires_ordering() {
    let Statement::Select(select) =
        first("SELECT id FROM employees ORDER BY department, id DESC FETCH FIRST 2 ROWS WITH TIES")
    else {
        panic!("not SELECT");
    };
    assert!(select.with_ties);
    assert_eq!(select.order_by.len(), 2);
    assert!(matches!(
        select.limit,
        Some(Expr::Literal(uqa_core::Value::Int(2)))
    ));

    let Statement::Select(null_count) =
        first("SELECT id FROM employees ORDER BY id FETCH FIRST NULL ROWS WITH TIES")
    else {
        panic!("not SELECT");
    };
    assert!(null_count.with_ties);
    assert!(matches!(
        null_count.limit,
        Some(Expr::Literal(uqa_core::Value::Null))
    ));

    let error = compile("SELECT id FROM employees FETCH FIRST 1 ROW WITH TIES").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(
        error.to_string(),
        "WITH TIES cannot be specified without ORDER BY clause"
    );

    let Statement::Select(values) =
        first("VALUES (1), (2), (2) ORDER BY 1 FETCH FIRST 2 ROWS WITH TIES")
    else {
        panic!("ordered VALUES must be a SELECT query block");
    };
    assert!(values.with_ties);
    assert!(matches!(values.from, Some(FromClause::Values { .. })));
    assert!(matches!(values.projections[0].expr, Expr::Star));
}
