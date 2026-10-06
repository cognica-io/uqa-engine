//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_column_drop_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-column-drop.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/foreign_column_drop_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    if provider == 0 {
        crate::pg18_oracle::verify(&engine, &durable.to_string());
    } else {
        drop(engine);
        let reopened = open(provider, &path);
        crate::pg18_oracle::verify(&reopened, &durable.to_string());
    }
}

#[test]
fn column_drop_keeps_foreign_row_values_and_rollback_restores_the_projection() {
    let engine = Engine::new();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(a integer,b integer,c text) SERVER source; PREPARE kept AS SELECT b,c FROM items", &[]).unwrap();
    engine
        .load_memory_foreign_table(
            "items",
            vec![super::row(&[
                ("a", uqa_core::Value::Int(1)),
                ("b", uqa_core::Value::Int(7)),
                ("c", uqa_core::Value::Str("kept".into())),
            ])],
        )
        .unwrap();
    engine
        .sql("BEGIN; ALTER FOREIGN TABLE items DROP a", &[])
        .unwrap();
    let kept = engine.sql("EXECUTE kept", &[]).unwrap();
    assert_eq!(
        kept.rows,
        vec![super::row(&[
            ("b", uqa_core::Value::Int(7)),
            ("c", uqa_core::Value::Str("kept".into())),
        ])]
    );
    let projection = engine.sql("SELECT * FROM items", &[]).unwrap();
    assert_eq!(projection.columns, ["b", "c"]);
    engine.sql("ROLLBACK", &[]).unwrap();
    let restored = engine.sql("SELECT a,b,c FROM items", &[]).unwrap();
    assert_eq!(restored.rows[0]["a"], uqa_core::Value::Int(1));
    assert_eq!(restored.rows[0]["b"], uqa_core::Value::Int(7));
    assert_eq!(restored.rows[0]["c"], uqa_core::Value::Str("kept".into()));
}
