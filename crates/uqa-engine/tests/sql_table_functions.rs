//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table functions in FROM + standalone `VALUES` + scalar table
//! function body. Mirrors the Python reference's
//! `_build_generate_series` / `_build_unnest` /
//! `_build_regexp_split_to_table` / `_build_json_each` /
//! `_build_json_array_elements` paths.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn generate_series_emits_inclusive_range() {
    let eng = Engine::new();
    let r = eng.sql("SELECT * FROM generate_series(1, 5)", &[]).unwrap();
    assert_eq!(r.rows.len(), 5);
}

#[test]
fn generate_series_with_step() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT * FROM generate_series(0, 10, 2)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 6);
}

#[test]
fn values_in_from_with_aliased_columns() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT id FROM (VALUES (1), (2), (3)) AS t(id)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn standalone_values_returns_rows() {
    let eng = Engine::new();
    let r = eng.sql("VALUES (1, 'a'), (2, 'b')", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].get("column1"), Some(&Value::Int(1)));
    assert_eq!(r.rows[1].get("column2"), Some(&Value::Str("b".into())));
}
