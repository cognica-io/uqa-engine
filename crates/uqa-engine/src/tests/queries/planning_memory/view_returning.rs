//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn automatic_view_returning_keeps_postgresql_star_order_and_row_images(#[case] provider: Provider) {
    // PostgreSQL 18.4 independently supplies the ordered fields, including MERGE's source-first wildcard and DELETE's absent NEW image.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-returning-owners.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer PRIMARY KEY, note text, bucket integer); CREATE VIEW visible AS SELECT note, id FROM items WHERE id > 0 WITH CASCADED CHECK OPTION", &[]).unwrap();
    for (sql, expected) in [
        ("INSERT INTO visible(id, note) VALUES (1, 'one') RETURNING *", vec![vec![text("one"), Value::Int(1)]]),
        ("UPDATE visible AS v SET note = s.note FROM (VALUES(1, 'updated', 99)) AS s(id, note, marker) WHERE v.id = s.id RETURNING *, s.*, old.note, new.note", vec![vec![text("updated"), Value::Int(1), Value::Int(1), text("updated"), Value::Int(99), Value::Int(1), text("updated"), Value::Int(99), text("one"), text("updated")]]),
        ("MERGE INTO visible AS v USING (VALUES(1, 'merged', 77), (2, 'inserted', 88)) AS s(id, note, marker) ON v.id = s.id WHEN MATCHED THEN UPDATE SET note = s.note WHEN NOT MATCHED THEN INSERT(id, note) VALUES (s.id, s.note) RETURNING *, s.*, old.note, new.note", vec![vec![Value::Int(1), text("merged"), Value::Int(77), text("merged"), Value::Int(1), Value::Int(1), text("merged"), Value::Int(77), text("updated"), text("merged")], vec![Value::Int(2), text("inserted"), Value::Int(88), text("inserted"), Value::Int(2), Value::Int(2), text("inserted"), Value::Int(88), Value::Null, text("inserted")]]),
        ("DELETE FROM visible AS v USING (VALUES(2, 'source', 55)) AS s(id, note, marker) WHERE v.id = s.id RETURNING WITH (OLD AS prior, NEW AS following) *, s.*, prior.note, following.note", vec![vec![text("inserted"), Value::Int(2), Value::Int(2), text("source"), Value::Int(55), Value::Int(2), text("source"), Value::Int(55), text("inserted"), Value::Null]]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), expected.len(), "{provider:?}: {sql}");
        assert_eq!(result.columns.len(), expected[0].len(), "{provider:?}: {sql}");
        for (row, values) in expected.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                assert_eq!(result.value_at(row, column), Some(value), "{provider:?}: {sql}");
            }
        }
    }
    check_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        check_rows(&engine);
        engine.close().unwrap();
    }
}

fn text(value: &str) -> Value {
    Value::Str(value.to_owned())
}

fn check_rows(engine: &Engine) {
    let rows = engine
        .sql("SELECT id, note FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(rows.value_at(0, 1), Some(&text("merged")));
}
