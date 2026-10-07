//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn select(sql: &str) -> Box<crate::ast::SelectStmt> {
    let Statement::Select(statement) = first(sql) else {
        panic!("SELECT expected")
    };
    statement
}

fn slot(expression: &Expr) -> usize {
    let Expr::WindowCall { spec, .. } = expression else {
        panic!("window call expected")
    };
    assert!(spec.reference.is_none());
    assert!(spec.raw_definition.is_none());
    spec.definition.expect("canonical slot")
}

#[test]
fn canonical_windows_preserve_own_clauses_and_postgresql_raw_matching() {
    let statement = select("SELECT sum(v) OVER second, row_number() OVER (ORDER BY v), row_number() OVER (ORDER BY v ASC), row_number() OVER (base), row_number() OVER derived FROM t WINDOW first AS (ORDER BY v), second AS (ORDER BY v), base AS (), derived AS (base), unused AS (PARTITION BY v)");
    assert_eq!(
        statement
            .windows
            .iter()
            .map(|window| window.name.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("first"),
            Some("second"),
            Some("base"),
            Some("derived"),
            Some("unused"),
            None
        ]
    );
    assert_eq!(
        statement
            .projections
            .iter()
            .map(|projection| slot(&projection.expr))
            .collect::<Vec<_>>(),
        [1, 0, 5, 3, 3]
    );
    assert_eq!(statement.windows[3].inherited, Some(2));
    assert_eq!(statement.windows[3].spec.partition_by.len(), 0);
    assert!(statement
        .windows
        .iter()
        .all(|window| window.spec.raw_definition.is_none()
            && window.spec.reference.is_none()
            && window.spec.definition.is_none()));
    let encoded = serde_json::to_value(&statement).unwrap();
    assert!(!encoded.to_string().contains("raw_definition"));
    assert_eq!(
        serde_json::from_value::<Box<crate::ast::SelectStmt>>(encoded).unwrap(),
        statement
    );
    let query = crate::plan::QueryPlan::lower(*statement);
    let encoded = serde_json::to_string(&query).unwrap();
    assert_eq!(
        serde_json::to_value(serde_json::from_str::<crate::plan::QueryPlan>(&encoded).unwrap())
            .unwrap(),
        serde_json::to_value(&query).unwrap()
    );
}

#[test]
fn canonical_window_comparison_ignores_locations_but_preserves_nested_names() {
    let statement = select("SELECT row_number() OVER (ORDER BY (SELECT v AS a FROM t)), row_number() OVER (ORDER BY (SELECT v AS b FROM t)), row_number() OVER (ORDER BY (SELECT v AS a FROM t))");
    assert_eq!(statement.windows.len(), 2);
    assert_eq!(
        statement
            .projections
            .iter()
            .map(|projection| slot(&projection.expr))
            .collect::<Vec<_>>(),
        [0, 1, 0]
    );
}

#[test]
fn canonical_window_definitions_reject_recursive_calls_without_crossing_query_scopes() {
    for sql in [
        "SELECT 1 WINDOW w AS (ORDER BY row_number() OVER w)",
        "SELECT row_number() OVER (PARTITION BY row_number() OVER ())",
    ] {
        let error = compile(sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P20"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            "window functions are not allowed in window definitions"
        );
    }
    let statement = select("SELECT 1 WINDOW w AS (ORDER BY (SELECT row_number() OVER ()))");
    assert_eq!(statement.windows.len(), 1);
}

#[test]
fn legacy_window_json_without_definitions_keeps_inline_execution_specs() {
    let mut value = serde_json::to_value(select("SELECT row_number() OVER (ORDER BY 1)")).unwrap();
    value.as_object_mut().unwrap().remove("windows");
    value["projections"][0]["expr"]["WindowCall"]["spec"]
        .as_object_mut()
        .unwrap()
        .remove("definition");
    let statement: crate::ast::SelectStmt = serde_json::from_value(value).unwrap();
    assert_eq!(statement.windows.len(), 0);
    let Expr::WindowCall { spec, .. } = &statement.projections[0].expr else {
        panic!("window")
    };
    assert!(spec.definition.is_none());
    assert_eq!(spec.order_by.len(), 1);
    let mut query = crate::plan::QueryPlan::lower(statement);
    let original = query.clone();
    query.normalize_window_definitions().unwrap();
    assert_eq!(
        serde_json::to_value(&query).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
}
