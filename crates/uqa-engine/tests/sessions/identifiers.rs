//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine adapters retain durable document reservations through SQL undo and table lifecycle changes.

use std::{collections::BTreeMap, path::Path, sync::Arc};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage::{PersistentStorageProvider, RelationIdentity, SchemaRow, TableSchema};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider,
};

#[derive(Clone, Copy, Debug)]
enum Layout {
    Native,
    KeyValue,
    Redb,
}

const LAYOUTS: [Layout; 3] = [Layout::Native, Layout::KeyValue, Layout::Redb];

fn open(layout: Layout, path: &Path) -> Engine {
    Engine::from_persistent_provider(provider(layout, path)).unwrap()
}

fn provider(layout: Layout, path: &Path) -> Arc<dyn PersistentStorageProvider> {
    match layout {
        Layout::Native => {
            let initialize = !path.exists();
            let connection = ManagedConnection::open(path).unwrap();
            if initialize {
                Catalog::open(connection.clone()).unwrap();
            }
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            Arc::new(SQLiteStorageProvider::new(connection))
        }
        Layout::KeyValue => Arc::new(SQLiteKeyValueStorage::open(path).unwrap()),
        Layout::Redb => Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()),
    }
}

fn assert_body(engine: &Engine, table: &str, id: u64, body: &str) {
    let row = engine.get_document(table, id).unwrap().unwrap();
    assert_eq!(row.get("body"), Some(&Value::Str(body.into())));
}

#[test]
fn opening_seeds_document_ids_from_legacy_reservations_and_existing_rows() {
    for layout in LAYOUTS {
        for legacy_next in [50, 500, u128::from(u64::MAX) + 1] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("legacy-document-ids.db");
            {
                let provider = provider(layout, &path);
                let session = provider.open_session().unwrap();
                session
                    .catalog
                    .save_schema_row(&SchemaRow::legacy("public"))
                    .unwrap();
                session.catalog.save_table(&TableSchema {
                    relation: RelationIdentity::new("public", "docs"),
                    security: uqa_storage::RelationSecurityRow::bootstrap(),
                    object_id: [21; 16],
                    storage_generation: [22; 16],
                    analyzer_json: serde_json::to_string(&uqa_analysis::Analyzer::default()).unwrap(),
                    fts_fields: Vec::new(),
                    vector_fields: Vec::new(),
                    columns_json: r#"[{"name":"id","ty":"Integer","primary_key":true,"not_null":true,"auto_increment":true},{"name":"body","ty":"Text","primary_key":false,"not_null":false,"auto_increment":false}]"#.into(),
                    constraints_json: "{}".into(),
                }).unwrap();
                session
                    .backend
                    .document_store("public.docs")
                    .put(
                        80,
                        BTreeMap::from([
                            ("id".into(), Value::Int(80)),
                            ("body".into(), Value::Str("legacy".into())),
                        ]),
                    )
                    .unwrap();
                session
                    .catalog
                    .set_metadata("uqa.table_next_id.v1:public.docs", &legacy_next.to_string())
                    .unwrap();
                let engine = Engine::from_persistent_provider(provider).unwrap();
                engine.sql("DELETE FROM docs", &[]).unwrap();
                assert_eq!(
                    session
                        .catalog
                        .get_metadata("uqa.table_next_id.v1:public.docs")
                        .unwrap()
                        .as_deref(),
                    Some("")
                );
            }
            let engine = open(layout, &path);
            let inserted = engine.sql("INSERT INTO docs (body) VALUES ('seeded')", &[]);
            if legacy_next > u128::from(u64::MAX) {
                assert!(inserted.is_err(), "{layout:?}");
            } else {
                inserted.unwrap();
                assert_body(
                    &engine,
                    "docs",
                    u64::try_from(legacy_next.max(81)).unwrap(),
                    "seeded",
                );
            }
        }
    }
}

#[test]
fn document_ids_survive_sql_undo_manual_observations_and_deleted_rows_on_reopen() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document-ids.db");
        {
            let engine = open(layout, &path);
            engine
                .sql(
                    "CREATE TABLE docs (body TEXT); INSERT INTO docs VALUES ('first')",
                    &[],
                )
                .unwrap();
            assert_body(&engine, "docs", 1, "first");
            let other = engine.new_session().unwrap();
            engine.sql("BEGIN; INSERT INTO docs VALUES ('discarded'); SAVEPOINT keep; INSERT INTO docs VALUES ('undone'); ROLLBACK TO SAVEPOINT keep; INSERT INTO docs VALUES ('branch'); ROLLBACK", &[]).unwrap();
            other
                .sql("INSERT INTO docs VALUES ('after rollback')", &[])
                .unwrap();
            assert_body(&other, "docs", 5, "after rollback");
            engine.sql("BEGIN", &[]).unwrap();
            engine
                .add_document(
                    "docs",
                    100,
                    BTreeMap::from([("body".into(), Value::Str("manual discarded".into()))]),
                )
                .unwrap();
            engine.sql("ROLLBACK", &[]).unwrap();
            other
                .sql(
                    "INSERT INTO docs VALUES ('after manual'); DELETE FROM docs",
                    &[],
                )
                .unwrap();
            assert!(other.get_document("docs", 101).unwrap().is_none());
        }
        let reopened = open(layout, &path);
        reopened
            .sql("INSERT INTO docs VALUES ('after reopen')", &[])
            .unwrap();
        assert_body(&reopened, "docs", 102, "after reopen");
    }
}

#[test]
fn document_ids_follow_table_generations_through_rename_truncate_and_name_reuse() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document-generations.db");
        {
            let engine = open(layout, &path);
            engine
                .sql(
                    "CREATE TABLE docs (body TEXT); INSERT INTO docs VALUES ('first')",
                    &[],
                )
                .unwrap();
            engine.sql("BEGIN; INSERT INTO docs VALUES ('discarded'); ROLLBACK; ALTER TABLE docs RENAME TO renamed; INSERT INTO renamed VALUES ('renamed')", &[]).unwrap();
            assert_body(&engine, "renamed", 3, "renamed");
            engine.sql("TRUNCATE renamed", &[]).unwrap();
        }
        {
            let engine = open(layout, &path);
            engine
                .sql("INSERT INTO renamed VALUES ('continued')", &[])
                .unwrap();
            assert_body(&engine, "renamed", 4, "continued");
            engine.sql("BEGIN; TRUNCATE renamed RESTART IDENTITY; INSERT INTO renamed VALUES ('private restart'); ROLLBACK; INSERT INTO renamed VALUES ('after rollback')", &[]).unwrap();
            assert_body(&engine, "renamed", 5, "after rollback");
            engine
                .sql("TRUNCATE renamed RESTART IDENTITY", &[])
                .unwrap();
        }
        let engine = open(layout, &path);
        engine
            .sql("INSERT INTO renamed VALUES ('restarted')", &[])
            .unwrap();
        assert_body(&engine, "renamed", 1, "restarted");
        engine.sql("DROP TABLE renamed; CREATE TABLE renamed (body TEXT); INSERT INTO renamed VALUES ('new object')", &[]).unwrap();
        assert_body(&engine, "renamed", 1, "new object");
        engine.sql("BEGIN READ ONLY", &[]).unwrap();
        assert!(engine
            .sql("INSERT INTO renamed VALUES ('forbidden')", &[])
            .is_err());
        engine
            .sql("ROLLBACK; INSERT INTO renamed VALUES ('permitted')", &[])
            .unwrap();
        assert_body(&engine, "renamed", 2, "permitted");
    }
}
