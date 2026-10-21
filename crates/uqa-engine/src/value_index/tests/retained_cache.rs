//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn open(kind: &str, path: &std::path::Path) -> crate::Engine {
    match kind {
        "memory" => crate::Engine::new(),
        "native" => crate::Engine::open(path).unwrap(),
        "sqlite_kv" => crate::Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        "redb" => crate::Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory("memory")]
#[case::native("native")]
#[case::sqlite_kv("sqlite_kv")]
#[case::redb("redb")]
fn cold_value_cache_keeps_bounded_payload_and_transaction_snapshots(#[case] kind: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retained.db");
    let engine = open(kind, &path);
    engine.sql("CREATE TABLE retained_keys (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO retained_keys SELECT g, g::text || repeat('x', 8192) FROM generate_series(1, 256) AS g; CREATE INDEX retained_body ON retained_keys (body)", &[]).unwrap();
    let engine = if kind == "memory" {
        engine
    } else {
        drop(engine);
        open(kind, &path)
    };
    engine.sql("SET work_mem = '64kB'", &[]).unwrap();
    let table = engine.require_table("retained_keys").unwrap();
    crate::Engine::value_indexes_clear(&table);
    let field = ValueIndexKey::Column("body".into());
    let allocation = allocation_counter::measure(|| {
        assert!(engine.ensure_value_index("retained_keys", &field).unwrap());
    });
    assert!(allocation.bytes_max < 1536 * 1024, "{kind}: {allocation:?}");
    let control = engine.query_retention_control().unwrap();
    let before = table.value_indexes.read()[&field].clone();
    assert!(before.is_spilled());
    let original = before
        .stored_value(1, &control)
        .unwrap()
        .unwrap()
        .into_owned();
    assert_eq!(original, Value::Str(format!("1{}", "x".repeat(8192))));
    engine.sql("BEGIN; UPDATE retained_keys SET body = 'new' WHERE id=1; SAVEPOINT s; DELETE FROM retained_keys WHERE id=2; ROLLBACK TO s", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT body FROM retained_keys WHERE id=1", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("new".into()))
    );
    assert_eq!(
        before.stored_value(1, &control).unwrap().as_deref(),
        Some(&original)
    );
    assert!(before.contains(2, &control).unwrap());
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT body FROM retained_keys WHERE id=1", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&original)
    );
    assert_eq!(
        engine
            .sql(
                "SELECT count(*) FROM retained_keys WHERE body IS NOT NULL",
                &[]
            )
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(256))
    );
}

#[rstest::rstest]
#[case::native("native")]
#[case::sqlite_kv("sqlite_kv")]
#[case::redb("redb")]
fn cold_value_cache_preserves_persisted_expression_values_after_routine_replacement(
    #[case] kind: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("expressions.db");
    let engine = open(kind, &path);
    engine.sql("CREATE FUNCTION retained_transform(x TEXT) RETURNS TEXT IMMUTABLE LANGUAGE SQL RETURN lower(x); CREATE TABLE expression_keys (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO expression_keys SELECT g, 'Key' || g::text || repeat('x',8192) FROM generate_series(1,8) AS g; CREATE INDEX transformed_key ON expression_keys (retained_transform(body))", &[]).unwrap();
    let field = engine
        .value_indexable_fields("expression_keys")
        .unwrap()
        .into_iter()
        .find(|field| matches!(field, ValueIndexKey::Index(_)))
        .unwrap();
    let expected = engine
        .storage
        .backend
        .as_ref()
        .unwrap()
        .load_btree_index("public.expression_keys", &field)
        .unwrap()
        .unwrap();
    engine.sql("CREATE OR REPLACE FUNCTION retained_transform(x TEXT) RETURNS TEXT IMMUTABLE LANGUAGE SQL RETURN upper(x)", &[]).unwrap();
    drop(engine);
    let engine = open(kind, &path);
    engine.sql("SET work_mem = '64kB'", &[]).unwrap();
    assert!(engine
        .ensure_value_index("expression_keys", &field)
        .unwrap());
    let table = engine.require_table("expression_keys").unwrap();
    let indexes = table.value_indexes.read();
    let index = &indexes[&field];
    assert!(index.is_spilled());
    for (id, expected) in expected {
        assert!(index
            .stored_value(id, &engine.query_retention_control().unwrap())
            .unwrap()
            .unwrap()
            .has_same_representation(&expected));
    }
}

#[rstest::rstest]
#[case::memory("memory")]
#[case::native("native")]
#[case::sqlite_kv("sqlite_kv")]
#[case::redb("redb")]
fn cold_detached_value_cache_reads_its_selected_document_generation(#[case] kind: &str) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(kind, &directory.path().join("detached.db"));
    engine.sql("CREATE TABLE selected_keys (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO selected_keys VALUES (1,'old'),(2,NULL); CREATE INDEX selected_body ON selected_keys (body)", &[]).unwrap();
    let live = engine.require_table("selected_keys").unwrap();
    let selected = engine.detach_query_table(&live, &live, None).unwrap();
    crate::Engine::value_indexes_clear(&selected);
    engine
        .sql(
            "UPDATE selected_keys SET body='new' WHERE id=1; DELETE FROM selected_keys WHERE id=2",
            &[],
        )
        .unwrap();
    let field = ValueIndexKey::Column("body".into());
    assert!(engine
        .ensure_query_value_index("selected_keys", &selected, &field)
        .unwrap());
    let control = engine.query_retention_control().unwrap();
    let indexes = selected.value_indexes.read();
    assert_eq!(
        indexes[&field]
            .stored_value(1, &control)
            .unwrap()
            .as_deref(),
        Some(&Value::Str("old".into()))
    );
    assert_eq!(
        indexes[&field]
            .stored_value(2, &control)
            .unwrap()
            .as_deref(),
        Some(&Value::Null)
    );
}
