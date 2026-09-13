//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the `uqa_highlight` SQL projection function.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::Engine;

#[path = "sql_uqa_highlight/explicit_analyzers.rs"]
mod explicit_analyzers;

#[path = "sql_uqa_highlight/runtime.rs"]
mod runtime;

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
fn highlight_null_inputs_do_not_call_later_arguments() {
    let eng = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    eng.register_scalar_function("highlight_tag", move |_args: &[Value]| {
        callback_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Str("<em>".into()))
    })
    .unwrap();

    for (expression, expected) in [
        (
            "uqa_highlight(NULL, highlight_tag(), highlight_tag())",
            Value::Null,
        ),
        (
            "uqa_highlight(body, NULL, highlight_tag())",
            Value::Str("the quick brown fox jumps over the lazy dog".into()),
        ),
    ] {
        let result = eng
            .sql(&format!("SELECT {expression} AS h FROM notes"), &[])
            .unwrap();
        assert_eq!(result.rows[0]["h"], expected);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    let result = eng
        .sql(
            "SELECT uqa_highlight(body, 'fox', highlight_tag(), '</em>') AS h FROM notes",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["h"],
        Value::Str("the quick brown <em>fox</em> jumps over the lazy dog".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
