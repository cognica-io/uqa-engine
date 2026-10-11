//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text-match field validation: searches against unknown columns,
//! unindexed columns, or computed expressions must fail with a clear
//! diagnostic instead of silently matching nothing.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{SQLError, SQLParam};

fn fusion_param() -> Vec<SQLParam> {
    vec![SQLParam::scalar(Value::Str("fusion".into()))]
}

fn engine_with_pages(indexed: bool) -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id INTEGER PRIMARY KEY, title TEXT, body TEXT, updated_at BIGINT)",
        &[],
    )
    .unwrap();
    if indexed {
        eng.sql(
            "CREATE INDEX pages_text ON pages USING gin (title, body)",
            &[],
        )
        .unwrap();
    }
    eng.sql(
        "INSERT INTO pages (id, title, body, updated_at) VALUES \
         (1, 'fusion scoring', 'fusion ranking in depth', 100), \
         (2, 'daily journal', 'one fusion mention only here', 200)",
        &[],
    )
    .unwrap();
    eng
}

fn expect_type_mismatch(result: Result<uqa_sql::SQLResult, SQLError>, needle: &str) {
    match result {
        Err(SQLError::TypeMismatch(message)) => {
            assert!(
                message.contains(needle),
                "expected `{needle}` in `{message}`"
            );
        }
        other => panic!("expected TypeMismatch containing `{needle}`, got {other:?}"),
    }
}

fn expect_text_index_required(
    result: Result<uqa_sql::SQLResult, SQLError>,
    expected_function: &str,
    expected_table: &str,
    expected_field: &str,
) {
    let error = result.expect_err("an unindexed text field must be rejected");
    match &error {
        SQLError::TextIndexRequired {
            function_name,
            table,
            field,
        } => {
            assert_eq!(function_name, expected_function);
            assert_eq!(table, expected_table);
            assert_eq!(field, expected_field);
        }
        other => panic!("expected TextIndexRequired, got {other:?}"),
    }
    assert_eq!(error.sqlstate(), Some("42804"));
    let message = error.to_string();
    assert!(message.contains("has no text index"), "{message}");
    assert!(
        message.contains(&format!(
            "CREATE INDEX ... ON {expected_table} USING gin ({expected_field})"
        )),
        "{message}"
    );
}

#[test]
fn unindexed_column_is_rejected_with_index_hint() {
    let eng = engine_with_pages(false);
    expect_text_index_required(
        eng.sql(
            "SELECT id FROM pages WHERE bayesian_match(body, $1)",
            &fusion_param(),
        ),
        "bayesian_match",
        "public.pages",
        "body",
    );
    expect_text_index_required(
        eng.sql(
            "SELECT id FROM pages WHERE multi_field_match(title, body, $1, 2.0, 1.0)",
            &fusion_param(),
        ),
        "multi_field_match",
        "public.pages",
        "title",
    );
}

#[test]
fn unknown_column_is_rejected() {
    let eng = engine_with_pages(true);
    expect_type_mismatch(
        eng.sql(
            "SELECT id FROM pages WHERE bayesian_match(no_such_column, $1)",
            &fusion_param(),
        ),
        "does not exist",
    );
}

#[test]
fn expression_fields_are_rejected_with_guidance() {
    let eng = engine_with_pages(true);
    expect_type_mismatch(
        eng.sql(
            "SELECT id FROM pages \
              WHERE multi_field_match(title, body || ' extra', $1, 2.0, 1.0)",
            &fusion_param(),
        ),
        "must be column references",
    );
}

#[test]
fn join_expression_fields_are_rejected() {
    let eng = engine_with_pages(true);
    eng.sql(
        "CREATE TABLE revs (id INTEGER PRIMARY KEY, page_id INTEGER, markdown TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO revs (id, page_id, markdown) VALUES (10, 1, 'fusion body')",
        &[],
    )
    .unwrap();
    expect_type_mismatch(
        eng.sql(
            "SELECT p.id FROM pages p LEFT JOIN revs r ON r.page_id = p.id \
              WHERE multi_field_match(p.title, p.body || ' ' || COALESCE(r.markdown, ''), $1, 2.0, 1.0)",
            &fusion_param(),
        ),
        "must be column references",
    );
    expect_text_index_required(
        eng.sql(
            "SELECT p.id FROM pages p LEFT JOIN revs r ON r.page_id = p.id \
              WHERE bayesian_match(r.markdown, $1)",
            &fusion_param(),
        ),
        "bayesian_match",
        "revs",
        "markdown",
    );
}

#[test]
fn all_pseudo_field_requires_some_indexed_column() {
    let eng = engine_with_pages(false);
    expect_type_mismatch(
        eng.sql(
            "SELECT id FROM pages WHERE text_match(_all, $1)",
            &fusion_param(),
        ),
        "no text-indexed columns",
    );
}

#[test]
fn valid_indexed_queries_keep_working() {
    let eng = engine_with_pages(true);
    let single = eng
        .sql(
            "SELECT id, _score FROM pages WHERE bayesian_match(body, $1) ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();
    assert_eq!(single.rows.len(), 2);
    let multi = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE multi_field_match(title, body, $1, 2.0, 1.0) ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();
    assert_eq!(multi.rows.len(), 2);
    let all = eng
        .sql(
            "SELECT id FROM pages WHERE text_match(_all, $1)",
            &fusion_param(),
        )
        .unwrap();
    assert_eq!(all.rows.len(), 2);
}

#[test]
fn jsonpath_fts_match_needs_no_text_index() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE jdocs (id INTEGER PRIMARY KEY, data JSONB)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO jdocs (id, data) VALUES \
         (1, '{\"a\":1}'::jsonb), (2, '{\"a\":2}'::jsonb)",
        &[],
    )
    .unwrap();
    let result = eng
        .sql(
            "SELECT id FROM jdocs WHERE data @@ '$.a == 2' ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
}
