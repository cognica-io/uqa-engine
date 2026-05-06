//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE TABLE AS SELECT` (CTAS) round-trip.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn ctas_materializes_select_into_new_table() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT, score INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO src (id, name, score) VALUES \
         (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE high AS SELECT id, name FROM src WHERE score >= 20",
        &[],
    )
    .unwrap();
    let r = eng.sql("SELECT id, name FROM high", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    let names: Vec<&Value> = r.rows.iter().filter_map(|row| row.get("name")).collect();
    assert!(names.contains(&&Value::Str("b".into())));
    assert!(names.contains(&&Value::Str("c".into())));
}

#[test]
fn ctas_with_if_not_exists_skips_when_present() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE src (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO src (id) VALUES (1), (2)", &[])
        .unwrap();
    eng.sql("CREATE TABLE dst AS SELECT id FROM src", &[])
        .unwrap();
    eng.sql("CREATE TABLE IF NOT EXISTS dst AS SELECT id FROM src", &[])
        .unwrap();
    let r = eng.sql("SELECT id FROM dst", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
}
