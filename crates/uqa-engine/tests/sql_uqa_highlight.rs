//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the `uqa_highlight` SQL projection function.

use uqa_core::Value;
use uqa_engine::Engine;

fn fixture() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (body) VALUES ('the quick brown fox jumps over the lazy dog')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn highlight_default_tags_wraps_match() {
    let eng = fixture();
    let res = eng
        .sql("SELECT uqa_highlight(body, 'fox') AS h FROM notes", &[])
        .unwrap();
    let h = match &res.rows[0]["h"] {
        Value::Str(s) => s.clone(),
        other => panic!("expected string, got {other:?}"),
    };
    assert_eq!(h, "the quick brown <b>fox</b> jumps over the lazy dog");
}

#[test]
fn highlight_custom_tags() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT uqa_highlight(body, 'fox', '<em>', '</em>') AS h FROM notes",
            &[],
        )
        .unwrap();
    let h = match &res.rows[0]["h"] {
        Value::Str(s) => s.clone(),
        other => panic!("expected string, got {other:?}"),
    };
    assert!(h.contains("<em>fox</em>"));
}

#[test]
fn highlight_fragment_extracts_window_around_match() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    let mut long = "padding ".repeat(80);
    long.push_str("a very specific phrase here ");
    long.push_str(&"padding ".repeat(80));
    eng.sql(
        "INSERT INTO notes (body) VALUES ($1)",
        &[uqa_engine::SQLParam::Scalar(Value::Str(long))],
    )
    .unwrap();

    let res = eng
        .sql(
            "SELECT uqa_highlight(body, 'specific phrase', '<b>', '</b>', 1, 60) AS h FROM notes",
            &[],
        )
        .unwrap();
    let h = match &res.rows[0]["h"] {
        Value::Str(s) => s.clone(),
        other => panic!("{other:?}"),
    };
    assert!(h.contains("<b>specific</b>") || h.contains("<b>phrase</b>"));
    assert!(h.starts_with("..."));
    assert!(h.ends_with("..."));
}
