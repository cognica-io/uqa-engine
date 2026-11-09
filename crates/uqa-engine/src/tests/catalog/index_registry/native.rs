//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, RelationIdentity, Value};
use uqa_execution::catalog::index::registry::CatalogIndexes;

fn bounded(engine: &Engine) -> MemoryBudget {
    let memory = MemoryBudget::new(1 << 20);
    let current = engine.durable.catalog_indexes.snapshot();
    let mut next = CatalogIndexes::with_budget(&memory);
    for row in current.values() {
        let relation = RelationIdentity::from_legacy_name(&row.table_name).unwrap();
        let persistence = engine.storage.tables.read()[&relation].persistence;
        next.insert(row.clone(), persistence).unwrap();
    }
    *engine.durable.catalog_indexes.write() = next;
    memory
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn failed_native_index_admission_preserves_catalog_rows_and_recovery(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("native-index.db"));
    engine.sql("CREATE TABLE items(id integer, kept integer); INSERT INTO items VALUES (7, 9); CREATE INDEX a_index ON items(id); CREATE INDEX b_index ON items(kept); CREATE INDEX c_index ON items(id, kept); CREATE FUNCTION identity_value(x integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT x $$; CREATE INDEX routine_index ON items(identity_value(id))", &[]).unwrap();
    let memory = bounded(&engine);
    let stored = || {
        engine
            .storage
            .catalog
            .as_ref()
            .map(|catalog| format!("{:?}", catalog.load_catalog_indexes().unwrap()))
    };
    let before = stored();
    for statement in [
        "CREATE INDEX added ON items(id)",
        "DROP INDEX b_index",
        "ALTER INDEX b_index RENAME TO renamed_index",
        "ALTER TABLE items RENAME COLUMN id TO renamed_id",
        "ALTER TABLE items RENAME TO renamed_items",
        "ALTER TABLE items DROP COLUMN id",
        "DROP TABLE items",
        "ALTER FUNCTION identity_value(integer) RENAME TO renamed_value",
    ] {
        let original = engine.durable.catalog_indexes.snapshot();
        let tables = engine.storage.tables.live_snapshot();
        let used = memory.used();
        let occupied = memory.reserve(memory.available()).unwrap();
        let error = engine.sql(statement, &[]).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some("53200"),
            "{provider:?}, {statement}: {error}"
        );
        let indexes = engine.durable.catalog_indexes.snapshot();
        assert!(
            uqa_core::memory::RetainedValue::ptr_eq(&original, &indexes),
            "{provider:?}, {statement}"
        );
        assert_eq!(original.len(), indexes.len());
        for (name, row) in original.iter() {
            assert!(
                std::ptr::eq(row, indexes.get(name).unwrap()),
                "{provider:?}, {statement}: original index {name:?}"
            );
        }
        drop(indexes);
        let after = engine.storage.tables.live_snapshot();
        assert_eq!(tables.len(), after.len());
        for (name, table) in tables.iter() {
            assert!(
                Arc::ptr_eq(table, &after[name]),
                "{provider:?}, {statement}: original table {name:?}"
            );
        }
        drop(after);
        assert_eq!(stored(), before, "{provider:?}, {statement}");
        drop(occupied);
        assert_eq!(memory.used(), used, "{provider:?}, {statement}");
        drop((original, tables));
        let result = engine
            .sql("SELECT identity_value(id) AS id, kept FROM items", &[])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(7));
        assert_eq!(result.rows[0]["kept"], Value::Int(9));
    }
    engine.sql("ALTER TABLE items RENAME COLUMN id TO usable; DROP INDEX b_index; INSERT INTO items VALUES (8, 10)", &[]).unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
